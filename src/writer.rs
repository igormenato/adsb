//! UART sink, position queue, and the write loop.

use std::io::{self, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use nix::poll::{PollFd, PollFlags};
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, TTYPort};

use trck::{
    ACK_LEN, AfterReply, CmdFeed, CmdScan, CmdStats, CmdStatus, EmergencyKind, FRAME_LEN, Fix,
    LAST_FIX_NONE, PowerOp, Queue, Table, check_positions, decide, pack_reply,
};

use super::sys;
use super::{CHECK_PAUSE, POLL_SLICE, Stop};

const QUEUE_CAP: usize = 64;

#[derive(Debug)]
pub(crate) enum ChunkError {
    Stopped,
    Failed(io::Error),
}

enum Sink {
    Uart(TTYPort),
    Stdout,
    #[cfg(test)]
    Buf(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingReply {
    bytes: [u8; ACK_LEN],
    sent: usize,
    after: AfterReply,
}

fn offer_reply(slot: &mut Option<PendingReply>, next: PendingReply) -> bool {
    if let Some(cur) = slot.as_ref()
        && (cur.sent > 0 || cur.after != AfterReply::None)
    {
        return false;
    }
    *slot = Some(next);
    true
}

pub(crate) struct Shared {
    queue: Mutex<Queue>,
    pub(crate) ready: Condvar,
    pub(crate) table: Mutex<Option<Table>>,
    started: Instant,
    frames_written: AtomicUsize,
    last_fix: Mutex<Option<Instant>>,
    pending_reply: Mutex<Option<PendingReply>>,
    allow_power: bool,
    paused: AtomicBool,
}

impl Shared {
    fn new(table: Option<Table>, allow_power: bool) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(Queue::new(QUEUE_CAP)),
            ready: Condvar::new(),
            table: Mutex::new(table),
            started: Instant::now(),
            frames_written: AtomicUsize::new(0),
            last_fix: Mutex::new(None),
            pending_reply: Mutex::new(None),
            allow_power,
            paused: AtomicBool::new(false),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn push(&self, fix: Fix) -> Option<Fix> {
        self.push_at(fix, SystemTime::now())
    }

    fn push_at(&self, fix: Fix, now: SystemTime) -> Option<Fix> {
        if self.paused.load(Ordering::Relaxed) {
            return None;
        }
        *self.last_fix.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        self.append_row_at(&fix, now);
        let dropped = self.lock().push(fix);
        self.ready.notify_one();
        dropped
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        if paused {
            self.lock().drop_waiting();
        }
        self.ready.notify_one();
    }

    fn stats(&self) -> CmdStats {
        let uptime_s = self.started.elapsed().as_secs().min(u64::from(u32::MAX)) as u32;
        let frames_sent = self
            .frames_written
            .load(Ordering::Relaxed)
            .min(u32::MAX as usize) as u32;
        let last_fix_age_s = match *self.last_fix.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(at) => at.elapsed().as_secs().min(u64::from(u32::MAX)) as u32,
            None => LAST_FIX_NONE,
        };
        CmdStats {
            uptime_s,
            frames_sent,
            last_fix_age_s,
        }
    }

    fn has_reply(&self) -> bool {
        self.pending_reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    fn queue_reply(&self, bytes: [u8; ACK_LEN], after: AfterReply) {
        let mut slot = self
            .pending_reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !offer_reply(
            &mut slot,
            PendingReply {
                bytes,
                sent: 0,
                after,
            },
        ) {
            eprintln!("cmd dropped, reply in flight");
        }
        drop(slot);
        self.ready.notify_one();
    }

    fn append_row_at(&self, fix: &Fix, now: SystemTime) {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(table) = table.as_mut() else {
            return;
        };
        if let Err(err) = table.append_at(fix, now) {
            eprintln!("table: {err}");
        }
    }
}

pub(crate) struct Writer {
    sink: Sink,
    stop: Arc<Stop>,
    pub(crate) shared: Arc<Shared>,
}

impl Writer {
    pub(crate) fn has_pending(&self) -> bool {
        self.shared.lock().has_pending() || self.shared.has_reply()
    }

    pub(crate) fn uart_fd(&self) -> Option<BorrowedFd<'_>> {
        match &self.sink {
            Sink::Uart(port) => Some(sys::as_fd(port)),
            Sink::Stdout => None,
            #[cfg(test)]
            Sink::Buf(_) => None,
        }
    }

    pub(crate) fn frames_written(&self) -> usize {
        self.shared.frames_written.load(Ordering::Relaxed)
    }

    pub(crate) fn enqueue(&mut self, fix: Fix) {
        self.enqueue_at(fix, SystemTime::now());
    }

    pub(crate) fn enqueue_at(&mut self, fix: Fix, now: SystemTime) {
        if let Some(dropped) = self.shared.push_at(fix, now) {
            log_drop(&dropped);
        }
    }

    pub(crate) fn write_some(&mut self) -> Result<(), ChunkError> {
        loop {
            if self.stop.is_set() {
                return Err(ChunkError::Stopped);
            }
            if !self.shared.lock().has_current() {
                if self.shared.has_reply() {
                    if !self.write_reply()? {
                        return Ok(());
                    }
                    continue;
                }
                if self.shared.is_paused() || !self.shared.lock().begin() {
                    return Ok(());
                }
            }
            if !self.write_frame()? {
                return Ok(());
            }
        }
    }

    fn write_frame(&mut self) -> Result<bool, ChunkError> {
        let mut buf = [0u8; FRAME_LEN];
        let n = {
            let q = self.shared.lock();
            let rest = q.unsent();
            buf[..rest.len()].copy_from_slice(rest);
            rest.len()
        };
        match write_sink(&mut self.sink, &buf[..n]) {
            Ok(0) => Err(ChunkError::Failed(io::Error::new(
                io::ErrorKind::WriteZero,
                "uart write wrote nothing",
            ))),
            Ok(wrote) => {
                if self.shared.lock().advance(wrote) {
                    self.shared.frames_written.fetch_add(1, Ordering::Relaxed);
                }
                Ok(true)
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => Ok(true),
            Err(err) => Err(ChunkError::Failed(err)),
        }
    }

    fn write_reply(&mut self) -> Result<bool, ChunkError> {
        let mut slot = self
            .shared
            .pending_reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(pending) = slot.as_mut() else {
            return Ok(false);
        };
        match write_sink(&mut self.sink, &pending.bytes[pending.sent..]) {
            Ok(0) => Err(ChunkError::Failed(io::Error::new(
                io::ErrorKind::WriteZero,
                "uart write wrote nothing",
            ))),
            Ok(n) => {
                pending.sent += n;
                if pending.sent < ACK_LEN {
                    return Ok(false);
                }
                let after = pending.after;
                *slot = None;
                drop(slot);
                drain_sink(&self.sink);
                match after {
                    AfterReply::None => {}
                    AfterReply::Power(op) => power(op),
                    AfterReply::Restart => {
                        drop(std::mem::replace(&mut self.sink, Sink::Stdout));
                        restart();
                    }
                }
                Ok(true)
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => Ok(true),
            Err(err) => Err(ChunkError::Failed(err)),
        }
    }

    fn wait_idle(&self, timeout: Duration) {
        if self.shared.has_reply() {
            return;
        }
        let q = self.shared.lock();
        if q.has_pending() {
            return;
        }
        match self.shared.ready.wait_timeout(q, timeout) {
            Ok(_) => {}
            Err(poison) => {
                let _ = poison.into_inner();
            }
        }
    }
}

pub(crate) fn log_drop(fix: &Fix) {
    let emergency = fix.aircraft.emergency.map(EmergencyKind::code).unwrap_or(0);
    eprintln!(
        "dropped frame icao={:06x} emergency={emergency}",
        fix.aircraft.icao
    );
}

pub(crate) fn log_fix(fix: &Fix) {
    eprintln!("fix icao={:06x}", fix.aircraft.icao);
}

fn write_sink(sink: &mut Sink, bytes: &[u8]) -> io::Result<usize> {
    match sink {
        Sink::Uart(port) => sys::write(sys::as_fd(port), bytes),
        Sink::Stdout => {
            let mut out = io::stdout().lock();
            out.write_all(bytes)?;
            out.flush()?;
            Ok(bytes.len())
        }
        #[cfg(test)]
        Sink::Buf(buf) => {
            buf.extend_from_slice(bytes);
            Ok(bytes.len())
        }
    }
}

fn drain_sink(sink: &Sink) {
    let Sink::Uart(port) = sink else {
        return;
    };
    if let Err(err) = nix::sys::termios::tcdrain(sys::as_fd(port)) {
        eprintln!("uart drain: {err}");
    }
}

fn restart() -> ! {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("restart: {err}");
            std::process::exit(78);
        }
    };
    eprintln!("restart");
    let err = Command::new(exe).args(std::env::args_os().skip(1)).exec();
    eprintln!("restart exec: {err}");
    std::process::exit(78);
}

fn power(op: PowerOp) {
    let arg = match op {
        PowerOp::Reboot => "reboot",
        PowerOp::Shutdown => "poweroff",
    };
    match Command::new("systemctl").arg(arg).status() {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("systemctl {arg} failed: {status}"),
        Err(err) => eprintln!("systemctl {arg}: {err}"),
    }
}

fn read_cmds(reader: TTYPort, shared: Arc<Shared>, stop: Arc<Stop>) {
    let mut scan = CmdScan::new();
    let mut tmp = [0u8; 64];
    while !stop.is_set() {
        match sys::read(sys::as_fd(&reader), &mut tmp) {
            Ok(0) => {
                if poll_in(&reader).is_err() {
                    return;
                }
            }
            Ok(n) => {
                for &byte in &tmp[..n] {
                    match scan.feed(byte) {
                        CmdFeed::None => {}
                        CmdFeed::BadCrc => eprintln!("cmd crc mismatch"),
                        CmdFeed::Cmd(cmd) => {
                            let (reply, after) =
                                decide(cmd, shared.allow_power, shared.is_paused(), shared.stats());
                            if reply.status == CmdStatus::Disabled {
                                eprintln!("power command disabled");
                            }
                            shared.set_paused(reply.paused);
                            shared.queue_reply(pack_reply(&reply), after);
                        }
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if poll_in(&reader).is_err() {
                    return;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                eprintln!("uart read: {err}");
                return;
            }
        }
    }
}

fn poll_in(reader: &TTYPort) -> io::Result<i32> {
    let mut poll_fd = PollFd::new(sys::as_fd(reader), PollFlags::POLLIN);
    sys::poll(std::slice::from_mut(&mut poll_fd), POLL_SLICE)
}

pub(crate) fn open_table(path: &Path) -> Option<Table> {
    match Table::open(path) {
        Ok(table) => Some(table),
        Err(err) => {
            eprintln!("cannot open {}: {err}", path.display());
            None
        }
    }
}

fn open_uart(
    path: &str,
    baud: u32,
    stop: Arc<Stop>,
    shared: Arc<Shared>,
) -> anyhow::Result<Writer> {
    let port = serialport::new(path, baud)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .timeout(Duration::ZERO)
        .open_native()
        .with_context(|| format!("cannot open {path}"))?;
    port.clear(ClearBuffer::All)
        .with_context(|| format!("cannot open {path}"))?;
    sys::set_nonblocking(sys::as_fd(&port)).with_context(|| format!("cannot open {path}"))?;
    sys::set_cloexec(sys::as_fd(&port)).with_context(|| format!("cannot open {path}"))?;
    let reader = port
        .try_clone_native()
        .with_context(|| format!("cannot clone {path}"))?;
    sys::set_cloexec(sys::as_fd(&reader)).with_context(|| format!("cannot clone {path}"))?;
    {
        let shared = Arc::clone(&shared);
        let stop = Arc::clone(&stop);
        thread::spawn(move || read_cmds(reader, shared, stop));
    }
    Ok(Writer {
        sink: Sink::Uart(port),
        stop,
        shared,
    })
}

pub(crate) fn open_output(
    path: &str,
    baud: u32,
    stop: Arc<Stop>,
    table: Option<Table>,
    allow_power: bool,
) -> anyhow::Result<Writer> {
    let shared = Shared::new(table, allow_power);
    if path == "-" {
        return Ok(Writer {
            sink: Sink::Stdout,
            stop,
            shared,
        });
    }
    open_uart(path, baud, stop, shared)
}

pub(crate) fn drain_writer(writer: &mut Writer, stop: &Stop) -> Result<(), ChunkError> {
    loop {
        writer.write_some()?;
        if !writer.has_pending() {
            return Ok(());
        }
        if stop.is_set() {
            return Err(ChunkError::Stopped);
        }
        let Some(fd) = writer.uart_fd() else {
            continue;
        };
        let mut poll_fd = PollFd::new(fd, PollFlags::POLLOUT);
        if let Err(err) = sys::poll(std::slice::from_mut(&mut poll_fd), POLL_SLICE) {
            return Err(ChunkError::Failed(err));
        }
    }
}

pub(crate) fn run_writer(mut writer: Writer, stop: &Stop, done: Option<&AtomicBool>) -> ExitCode {
    let mut drain_from = None;
    loop {
        if stop.is_set() {
            eprintln!("stopped");
            return ExitCode::SUCCESS;
        }
        if let Err(err) = writer.write_some() {
            return write_exit(err);
        }
        let finished = done.is_some_and(|flag| flag.load(Ordering::Relaxed));
        if finished && !writer.has_pending() {
            return ExitCode::SUCCESS;
        }
        if finished {
            let started = drain_from.get_or_insert_with(Instant::now);
            if started.elapsed() >= Duration::from_secs(2) {
                return ExitCode::SUCCESS;
            }
        }
        if writer.has_pending() {
            if let Some(fd) = writer.uart_fd() {
                let mut poll_fd = PollFd::new(fd, PollFlags::POLLOUT);
                if let Err(err) = sys::poll(std::slice::from_mut(&mut poll_fd), POLL_SLICE) {
                    eprintln!("uart poll: {err}");
                    return ExitCode::from(1);
                }
            }
            continue;
        }
        writer.wait_idle(POLL_SLICE);
    }
}

pub(crate) fn run_check(writer: &mut Writer, stop: &Stop) -> ExitCode {
    while !stop.is_set() {
        for position in check_positions() {
            log_fix(&position);
            writer.enqueue(position);
        }
        let deadline = Instant::now() + CHECK_PAUSE;
        loop {
            if stop.is_set() {
                break;
            }
            if let Err(err) = writer.write_some() {
                return write_exit(err);
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if writer.has_pending() {
                let slice = deadline.saturating_duration_since(now).min(POLL_SLICE);
                if let Some(fd) = writer.uart_fd() {
                    let mut poll_fd = PollFd::new(fd, PollFlags::POLLOUT);
                    if let Err(err) = sys::poll(std::slice::from_mut(&mut poll_fd), slice) {
                        eprintln!("uart poll: {err}");
                        return ExitCode::from(1);
                    }
                }
                continue;
            }
            writer.wait_idle(deadline.saturating_duration_since(now).min(POLL_SLICE));
        }
    }
    eprintln!("stopped");
    ExitCode::SUCCESS
}

pub(crate) fn write_exit(err: ChunkError) -> ExitCode {
    match err {
        ChunkError::Stopped => {
            eprintln!("stopped");
            ExitCode::SUCCESS
        }
        ChunkError::Failed(err) => {
            eprintln!("uart write failed: {err}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trck::Cmd;

    fn ping() -> PendingReply {
        PendingReply {
            bytes: [0; ACK_LEN],
            sent: 0,
            after: AfterReply::None,
        }
    }

    fn reboot() -> PendingReply {
        PendingReply {
            bytes: [1; ACK_LEN],
            sent: 0,
            after: AfterReply::Power(PowerOp::Reboot),
        }
    }

    #[test]
    fn in_flight_reply_is_not_replaced() {
        let mut slot = Some(PendingReply { sent: 4, ..ping() });
        assert!(!offer_reply(&mut slot, reboot()));
        let got = slot.unwrap();
        assert_eq!(got.sent, 4);
        assert_eq!(got.after, AfterReply::None);

        let mut slot = Some(PendingReply {
            sent: 4,
            ..reboot()
        });
        assert!(!offer_reply(&mut slot, ping()));
        let got = slot.unwrap();
        assert_eq!(got.after, AfterReply::Power(PowerOp::Reboot));
        assert_eq!(got.sent, 4);

        let mut slot = Some(reboot());
        assert!(!offer_reply(&mut slot, ping()));
        assert_eq!(slot.unwrap().after, AfterReply::Power(PowerOp::Reboot));

        let mut slot = Some(ping());
        assert!(offer_reply(&mut slot, reboot()));
        assert_eq!(slot.unwrap().after, AfterReply::Power(PowerOp::Reboot));
    }

    fn buf_writer(table: Option<Table>) -> Writer {
        Writer {
            sink: Sink::Buf(Vec::new()),
            stop: Stop::new(),
            shared: Shared::new(table, false),
        }
    }

    fn buf(writer: &Writer) -> &[u8] {
        match &writer.sink {
            Sink::Buf(bytes) => bytes,
            _ => panic!("expected buf sink"),
        }
    }

    fn csv_rows(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .skip(1)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn paused_writer_skips_frames_and_writes_replies() {
        let path = std::env::temp_dir().join(format!(
            "trck-pause-{}-{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let mut writer = buf_writer(Some(Table::open(&path).expect("table")));
        writer.enqueue(check_positions()[0]);
        writer.write_some().expect("write");
        assert_eq!(buf(&writer).len(), FRAME_LEN);
        assert_eq!(csv_rows(&path).len(), 1);

        writer.shared.set_paused(true);
        thread::sleep(Duration::from_millis(1100));
        let age = writer.shared.stats().last_fix_age_s;
        assert!(age >= 1, "{age}");
        writer.enqueue(check_positions()[1]);
        writer.write_some().expect("write");
        assert_eq!(buf(&writer).len(), FRAME_LEN);
        assert_eq!(csv_rows(&path).len(), 1);
        thread::sleep(Duration::from_millis(1100));
        writer.enqueue(check_positions()[2]);
        let age2 = writer.shared.stats().last_fix_age_s;
        assert!(age2 > age, "age={age} age2={age2}");
        assert_eq!(csv_rows(&path).len(), 1);

        let (reply, after) = decide(Cmd::Status as u8, false, true, writer.shared.stats());
        assert!(reply.paused);
        assert_eq!(after, AfterReply::None);
        writer.shared.queue_reply(pack_reply(&reply), after);
        writer.write_some().expect("write");
        let out = buf(&writer);
        assert_eq!(out.len(), FRAME_LEN + ACK_LEN);
        assert_eq!(&out[FRAME_LEN..FRAME_LEN + 4], b"TACK");
        assert_eq!(out[FRAME_LEN + 18], 1);

        writer.shared.set_paused(false);
        writer.enqueue(check_positions()[1]);
        writer.write_some().expect("write");
        assert_eq!(buf(&writer).len(), FRAME_LEN * 2 + ACK_LEN);
        assert_eq!(csv_rows(&path).len(), 2);
        assert_eq!(writer.shared.stats().last_fix_age_s, 0);
        let _ = std::fs::remove_file(&path);
    }
}
