//! UART sink, position queue, and the write loop.

use std::io::{self, Write};
use std::os::fd::BorrowedFd;
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
    ACK_LEN, CmdFeed, CmdScan, CmdStats, CmdStatus, EmergencyKind, FRAME_LEN, Fix, LAST_FIX_NONE,
    PowerOp, Queue, Table, check_positions, decide, pack_reply,
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
}

struct PendingReply {
    bytes: [u8; ACK_LEN],
    sent: usize,
    power: Option<PowerOp>,
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
        })
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn push(&self, fix: Fix) -> Option<Fix> {
        self.push_at(fix, SystemTime::now())
    }

    fn push_at(&self, fix: Fix, now: SystemTime) -> Option<Fix> {
        let dropped = self.lock().push(fix);
        *self.last_fix.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        self.ready.notify_one();
        self.append_row_at(&fix, now);
        dropped
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

    fn queue_reply(&self, bytes: [u8; ACK_LEN], power: Option<PowerOp>) {
        *self
            .pending_reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(PendingReply {
            bytes,
            sent: 0,
            power,
        });
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
                if !self.shared.lock().begin() {
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
                let op = pending.power.take();
                *slot = None;
                drop(slot);
                drain_sink(&self.sink);
                if let Some(op) = op {
                    power(op);
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
                            let (reply, power) = decide(cmd, shared.allow_power, shared.stats());
                            if reply.status == CmdStatus::Disabled {
                                eprintln!("power command disabled");
                            }
                            shared.queue_reply(pack_reply(&reply), power);
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
    let reader = port
        .try_clone_native()
        .with_context(|| format!("cannot clone {path}"))?;
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
