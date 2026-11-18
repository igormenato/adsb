//! UART sink, position queue, and the write loop.

use std::collections::VecDeque;
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
    ACK_LEN, AfterReply, CmdFeed, CmdScan, CmdStats, CmdStatus, EmergencyKind, FRAME_LEN, Fix,
    LAST_FIX_NONE, PowerOp, Queue, Table, check_positions, decide, pack_reply,
};

use super::sys;
use super::{CHECK_PAUSE, POLL_SLICE, Stop, sleep_until_stop};

const QUEUE_CAP: usize = 64;
const REPLY_CAP: usize = 8;
const UART_RETRY_MIN: Duration = Duration::from_millis(50);
const UART_RETRY_MAX: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub(crate) enum ChunkError {
    Stopped,
    Restart,
    Failed(io::Error),
}

enum Sink {
    Uart(TTYPort),
    Closed,
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

pub(crate) struct Shared {
    queue: Mutex<Queue>,
    pub(crate) ready: Condvar,
    pub(crate) table: Mutex<Option<Table>>,
    started: Instant,
    frames_written: AtomicUsize,
    last_fix: Mutex<Option<Instant>>,
    pending_reply: Mutex<VecDeque<PendingReply>>,
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
            pending_reply: Mutex::new(VecDeque::new()),
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
        let mut q = self.lock();
        if self.paused.load(Ordering::Relaxed) {
            return None;
        }
        *self.last_fix.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        self.append_row_at(&fix, now);
        let dropped = q.push(fix);
        drop(q);
        self.ready.notify_one();
        dropped
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    fn set_paused(&self, paused: bool) {
        let mut q = self.lock();
        self.paused.store(paused, Ordering::Relaxed);
        if paused {
            q.drop_waiting();
        }
        drop(q);
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
        !self
            .pending_reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    fn can_queue(&self) -> bool {
        self.pending_reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
            < REPLY_CAP
    }

    fn queue_reply(&self, bytes: [u8; ACK_LEN], after: AfterReply) -> bool {
        let mut q = self
            .pending_reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if q.len() >= REPLY_CAP {
            return false;
        }
        q.push_back(PendingReply {
            bytes,
            sent: 0,
            after,
        });
        drop(q);
        self.ready.notify_one();
        true
    }

    fn handle_cmd(&self, cmd: u8) {
        if !self.can_queue() {
            eprintln!("cmd dropped, reply queue full");
            return;
        }
        let (mut reply, mut after) = decide(cmd, self.allow_power, self.is_paused(), self.stats());
        if reply.status == CmdStatus::Disabled {
            eprintln!("power command disabled");
        }
        if let AfterReply::Power(op) = after {
            match try_power(op) {
                Ok(()) => after = AfterReply::None,
                Err(err) => {
                    eprintln!("{err}");
                    reply.status = CmdStatus::Failed;
                    after = AfterReply::None;
                }
            }
        }
        self.set_paused(reply.paused);
        let _ = self.queue_reply(pack_reply(&reply), after);
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
    uart: Option<(String, u32)>,
    retry_wait: Duration,
    retry_at: Instant,
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
            Sink::Closed | Sink::Stdout => None,
            #[cfg(test)]
            Sink::Buf(_) => None,
        }
    }

    pub(crate) fn recover_uart(&mut self, why: &str) {
        if matches!(self.sink, Sink::Uart(_)) {
            eprintln!("{why}");
            self.sink = Sink::Closed;
            self.retry_at = Instant::now() + self.retry_wait;
        }
    }

    pub(crate) fn try_reopen_uart(&mut self) {
        let Some((path, baud)) = self.uart.clone() else {
            return;
        };
        if !matches!(self.sink, Sink::Closed) || Instant::now() < self.retry_at {
            return;
        }
        match open_port(&path, baud) {
            Ok(port) => {
                eprintln!("uart reopened");
                self.sink = Sink::Uart(port);
                self.retry_wait = UART_RETRY_MIN;
            }
            Err(err) => {
                eprintln!("uart reopen: {err:#}");
                self.retry_wait = (self.retry_wait * 2).min(UART_RETRY_MAX);
                self.retry_at = Instant::now() + self.retry_wait;
            }
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
        self.try_reopen_uart();
        if matches!(self.sink, Sink::Closed) {
            return Ok(());
        }
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
            Ok(0) => {
                self.recover_uart("uart write wrote nothing");
                Ok(false)
            }
            Ok(wrote) => {
                if self.shared.lock().advance(wrote) {
                    self.shared.frames_written.fetch_add(1, Ordering::Relaxed);
                }
                Ok(true)
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => Ok(true),
            Err(err) => {
                self.recover_uart(&format!("uart write failed: {err}"));
                Ok(false)
            }
        }
    }

    fn write_reply(&mut self) -> Result<bool, ChunkError> {
        let mut slot = self
            .shared
            .pending_reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(pending) = slot.front_mut() else {
            return Ok(false);
        };
        match write_sink(&mut self.sink, &pending.bytes[pending.sent..]) {
            Ok(0) => {
                drop(slot);
                self.recover_uart("uart write wrote nothing");
                Ok(false)
            }
            Ok(n) => {
                pending.sent += n;
                if pending.sent < ACK_LEN {
                    return Ok(false);
                }
                let after = pending.after;
                slot.pop_front();
                drop(slot);
                drain_sink(&self.sink);
                match after {
                    AfterReply::None | AfterReply::Power(_) => Ok(true),
                    AfterReply::Restart => Err(ChunkError::Restart),
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => Ok(true),
            Err(err) => {
                drop(slot);
                self.recover_uart(&format!("uart write failed: {err}"));
                Ok(false)
            }
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
        Sink::Closed => Err(io::Error::new(io::ErrorKind::BrokenPipe, "uart closed")),
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

fn power_arg(op: PowerOp) -> &'static str {
    match op {
        PowerOp::Reboot => "reboot",
        PowerOp::Shutdown => "poweroff",
    }
}

fn systemctl_power(arg: &str) -> io::Result<bool> {
    match Command::new("systemctl").arg(arg).status() {
        Ok(status) => Ok(status.success()),
        Err(err) => Err(err),
    }
}

static POWER: Mutex<fn(&str) -> io::Result<bool>> = Mutex::new(systemctl_power);

fn try_power(op: PowerOp) -> Result<(), String> {
    let arg = power_arg(op);
    let run = *POWER.lock().unwrap_or_else(PoisonError::into_inner);
    match run(arg) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("systemctl {arg} failed")),
        Err(err) => Err(format!("systemctl {arg}: {err}")),
    }
}

fn read_cmds(mut reader: TTYPort, shared: Arc<Shared>, stop: Arc<Stop>, path: String, baud: u32) {
    let mut scan = CmdScan::new();
    let mut tmp = [0u8; 64];
    let mut wait = UART_RETRY_MIN;
    while !stop.is_set() {
        let lost = match sys::read(sys::as_fd(&reader), &mut tmp) {
            Ok(0) => Some("uart hangup"),
            Ok(n) => {
                for &byte in &tmp[..n] {
                    match scan.feed(byte) {
                        CmdFeed::None => {}
                        CmdFeed::BadCrc => eprintln!("cmd crc mismatch"),
                        CmdFeed::Cmd(cmd) => shared.handle_cmd(cmd),
                    }
                }
                None
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                poll_cmds(&reader).err().map(|_| "uart hangup")
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => None,
            Err(_) => Some("uart read"),
        };
        let Some(why) = lost else {
            continue;
        };
        eprintln!("{why}");
        drop(reader);
        loop {
            if stop.is_set() {
                return;
            }
            sleep_until_stop(&stop, wait);
            wait = (wait * 2).min(UART_RETRY_MAX);
            match open_port(&path, baud) {
                Ok(port) => {
                    eprintln!("uart reopened");
                    reader = port;
                    wait = UART_RETRY_MIN;
                    break;
                }
                Err(err) => eprintln!("uart reopen: {err:#}"),
            }
        }
    }
}

fn poll_cmds(reader: &TTYPort) -> io::Result<()> {
    let mut poll_fd = PollFd::new(
        sys::as_fd(reader),
        PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR,
    );
    sys::poll(std::slice::from_mut(&mut poll_fd), POLL_SLICE)?;
    let ev = poll_fd.revents().unwrap_or_else(PollFlags::empty);
    if ev.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
        return Err(io::Error::new(io::ErrorKind::BrokenPipe, "hangup"));
    }
    Ok(())
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

fn open_port(path: &str, baud: u32) -> anyhow::Result<TTYPort> {
    let port = serialport::new(path, baud)
        .exclusive(false)
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
    Ok(port)
}

fn open_uart(
    path: &str,
    baud: u32,
    stop: Arc<Stop>,
    shared: Arc<Shared>,
) -> anyhow::Result<Writer> {
    let port = open_port(path, baud)?;
    let reader = port
        .try_clone_native()
        .with_context(|| format!("cannot clone {path}"))?;
    sys::set_nonblocking(sys::as_fd(&reader)).with_context(|| format!("cannot clone {path}"))?;
    sys::set_cloexec(sys::as_fd(&reader)).with_context(|| format!("cannot clone {path}"))?;
    {
        let shared = Arc::clone(&shared);
        let stop = Arc::clone(&stop);
        let path = path.to_string();
        thread::spawn(move || read_cmds(reader, shared, stop, path, baud));
    }
    Ok(Writer {
        sink: Sink::Uart(port),
        uart: Some((path.to_string(), baud)),
        retry_wait: UART_RETRY_MIN,
        retry_at: Instant::now(),
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
            uart: None,
            retry_wait: UART_RETRY_MIN,
            retry_at: Instant::now(),
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
            match err {
                ChunkError::Stopped | ChunkError::Restart => return write_exit(err),
                ChunkError::Failed(err) => {
                    writer.recover_uart(&format!("uart write failed: {err}"))
                }
            }
        }
        writer.try_reopen_uart();
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
                    writer.recover_uart(&format!("uart poll: {err}"));
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
                match err {
                    ChunkError::Stopped | ChunkError::Restart => return write_exit(err),
                    ChunkError::Failed(err) => {
                        writer.recover_uart(&format!("uart write failed: {err}"))
                    }
                }
            }
            writer.try_reopen_uart();
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if writer.has_pending() {
                let slice = deadline.saturating_duration_since(now).min(POLL_SLICE);
                if let Some(fd) = writer.uart_fd() {
                    let mut poll_fd = PollFd::new(fd, PollFlags::POLLOUT);
                    if let Err(err) = sys::poll(std::slice::from_mut(&mut poll_fd), slice) {
                        writer.recover_uart(&format!("uart poll: {err}"));
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
        ChunkError::Restart => {
            eprintln!("restart");
            ExitCode::from(1)
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

    fn ack_status(bytes: &[u8; ACK_LEN]) -> CmdStatus {
        match bytes[5] {
            0 => CmdStatus::Ok,
            1 => CmdStatus::Disabled,
            2 => CmdStatus::Unknown,
            3 => CmdStatus::Failed,
            other => panic!("status {other}"),
        }
    }

    fn pending(shared: &Shared) -> Vec<PendingReply> {
        shared
            .pending_reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    fn with_power(run: fn(&str) -> io::Result<bool>, body: impl FnOnce()) {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let prev = {
            let mut slot = POWER.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::replace(&mut *slot, run)
        };
        body();
        *POWER.lock().unwrap_or_else(PoisonError::into_inner) = prev;
    }

    #[test]
    fn pause_and_push_share_the_queue_lock() {
        let shared = Shared::new(None, false);
        thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..2_000 {
                    let _ = shared.push(check_positions()[0]);
                }
            });
            scope.spawn(|| {
                for _ in 0..2_000 {
                    shared.set_paused(true);
                    shared.set_paused(false);
                }
            });
        });
        shared.set_paused(true);
        assert!(!shared.lock().has_pending());
        assert!(shared.push(check_positions()[0]).is_none());
        assert!(!shared.lock().has_pending());
    }

    #[test]
    fn reboot_acks_after_systemctl() {
        with_power(
            |_| Ok(true),
            || {
                let shared = Shared::new(None, true);
                shared.handle_cmd(Cmd::Reboot as u8);
                let got = pending(&shared);
                assert_eq!(got.len(), 1);
                assert_eq!(ack_status(&got[0].bytes), CmdStatus::Ok);
                assert_eq!(got[0].after, AfterReply::None);
            },
        );
        with_power(
            |_| Ok(false),
            || {
                let shared = Shared::new(None, true);
                shared.handle_cmd(Cmd::Reboot as u8);
                let got = pending(&shared);
                assert_eq!(got.len(), 1);
                assert_eq!(ack_status(&got[0].bytes), CmdStatus::Failed);
                assert_eq!(got[0].after, AfterReply::None);
            },
        );
        with_power(
            |_| Err(io::Error::other("missing")),
            || {
                let shared = Shared::new(None, true);
                shared.handle_cmd(Cmd::Shutdown as u8);
                let got = pending(&shared);
                assert_eq!(ack_status(&got[0].bytes), CmdStatus::Failed);
            },
        );
    }

    #[test]
    fn second_command_is_queued() {
        let shared = Shared::new(None, false);
        shared.handle_cmd(Cmd::Pause as u8);
        shared.handle_cmd(Cmd::Status as u8);
        let got = pending(&shared);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].bytes[4], Cmd::Pause as u8);
        assert_eq!(got[0].bytes[18], 1);
        assert_eq!(got[1].bytes[4], Cmd::Status as u8);
        assert_eq!(got[1].bytes[18], 1);
        assert!(shared.is_paused());
    }

    fn buf_writer(table: Option<Table>) -> Writer {
        Writer {
            sink: Sink::Buf(Vec::new()),
            uart: None,
            retry_wait: UART_RETRY_MIN,
            retry_at: Instant::now(),
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
    fn hangup_retries_without_stopping() {
        use serialport::SerialPort;
        let (master, slave) = TTYPort::pair().expect("pty");
        let path = slave.name().expect("pty name");
        sys::set_nonblocking(sys::as_fd(&slave)).expect("nonblock");
        let stop = Stop::new();
        let shared = Shared::new(None, false);
        let stop_thread = Arc::clone(&stop);
        let thread = thread::spawn(move || read_cmds(slave, shared, stop_thread, path, 115_200));
        drop(master);
        thread::sleep(Duration::from_millis(200));
        assert!(!stop.is_set());
        stop.request();
        thread.join().expect("reader");
    }

    #[test]
    fn restart_acks_then_returns_restart() {
        let mut writer = buf_writer(None);
        let (reply, after) = decide(Cmd::Restart as u8, false, true, writer.shared.stats());
        assert_eq!(after, AfterReply::Restart);
        assert!(!reply.paused);
        assert!(writer.shared.queue_reply(pack_reply(&reply), after));
        let err = writer.write_some().expect_err("restart");
        assert!(matches!(err, ChunkError::Restart));
        let out = buf(&writer);
        assert_eq!(&out[..4], b"TACK");
        assert_eq!(out[4], Cmd::Restart as u8);
    }

    #[test]
    fn extra_commands_are_dropped_when_replies_are_full() {
        let shared = Shared::new(None, false);
        for _ in 0..REPLY_CAP {
            shared.handle_cmd(Cmd::Ping as u8);
        }
        let mut got = pending(&shared);
        assert_eq!(got.len(), REPLY_CAP);
        got[0].sent = 4;
        got[1].after = AfterReply::Restart;
        {
            let mut q = shared
                .pending_reply
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *q = got.into();
        }
        shared.handle_cmd(Cmd::Pause as u8);
        let got = pending(&shared);
        assert_eq!(got.len(), REPLY_CAP);
        assert_eq!(got[0].sent, 4);
        assert_eq!(got[0].bytes[4], Cmd::Ping as u8);
        assert_eq!(got[1].after, AfterReply::Restart);
        assert!(!shared.is_paused());
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
