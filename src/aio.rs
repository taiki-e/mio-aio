use libc::{c_int, off_t};
use mio::{Evented, Poll, Token, Ready, PollOpt};
use mio::unix::UnixReady;
use nix;
use nix::sys::aio;
use nix::sys::signal::SigevNotify;
use std::cell::{Cell, RefCell};
use std::io;
use std::iter::FromIterator;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::RawFd;
use std::rc::Rc;


#[derive(Debug)]
pub struct AioCb<'a> {
    // Must use a RefCell because mio::Evented's methods only take immutable
    // references, and unlike sockets, registering aiocb's requires modifying
    // the aiocb.  Must use Box for the AioCb so its location in memory will be
    // constant.  It is an error to move a libc::aiocb after passing it to the
    // kernel.
    inner: RefCell<Box<aio::AioCb<'a>>>
}

/// Wrapper around nix::sys::aio::AioCb.
///
/// Implements mio::Evented.  After creation, use mio::Evented::register to
/// connect to the event loop
impl<'a> AioCb<'a> {
    /// Wraps nix::sys::aio::AioCb::from_fd.
    pub fn from_fd(fd: RawFd, prio: c_int) -> AioCb<'a> {
        let aiocb = aio::AioCb::from_fd(fd, prio, SigevNotify::SigevNone);
        AioCb { inner: RefCell::new(Box::new(aiocb)) }
    }

    /// Wraps nix::sys::aio::AioCb::from_mut_slice.
    pub fn from_boxed_slice(fd: RawFd, offs: off_t, buf: Rc<Box<[u8]>>,
                          prio: c_int, opcode: aio::LioOpcode) -> AioCb<'a>{
        let aiocb = aio::AioCb::from_boxed_slice(fd, offs, buf, prio,
                                               SigevNotify::SigevNone, opcode);
        AioCb { inner: RefCell::new(Box::new(aiocb)) }
    }

    /// Wraps nix::sys::aio::from_slice
    pub fn from_slice(fd: RawFd, offs: off_t, buf: &'a [u8],
                      prio: c_int, opcode: aio::LioOpcode) -> AioCb {
        let aiocb = aio::AioCb::from_slice(fd, offs, buf, prio,
                                           SigevNotify::SigevNone, opcode);
        AioCb { inner: RefCell::new(Box::new(aiocb)) }
    }

    /// Wrapper for nix::sys::aio::aio_return
    pub fn aio_return(&self) -> nix::Result<isize> {
        self.inner.borrow_mut().aio_return()
    }

    /// Wrapper for nix::sys::aio::AioCb::cancel
    pub fn cancel(&self) -> nix::Result<aio::AioCancelStat> {
        self.inner.borrow_mut().cancel()
    }

    /// Wrapper for nix::sys::aio::AioCb::fsync
    pub fn fsync(&self, mode: aio::AioFsyncMode) -> nix::Result<()> {
        self.inner.borrow_mut().fsync(mode)
    }

    /// Wrapper for nix::sys::aio::AioCb::read
    pub fn read(&self) -> nix::Result<()> {
        self.inner.borrow_mut().read()
    }

    /// Wrapper for nix::sys::aio::AioCb::write
    pub fn write(&self) -> nix::Result<()> {
        self.inner.borrow_mut().write()
    }
}

impl<'a> Evented for AioCb<'a> {
    fn register(&self,
                poll: &Poll,
                token: Token,
                events: Ready,
                _: PollOpt) -> io::Result<()> {
        assert!(UnixReady::from(events).is_aio());
        let udata = usize::from(token);
        let kq = poll.as_raw_fd();
        let sigev = SigevNotify::SigevKevent{kq: kq, udata: udata as isize};
        self.inner.borrow_mut().set_sigev_notify(sigev);
        Ok(())
    }

    fn reregister(&self,
                  poll: &Poll,
                  token: Token,
                  events: Ready,
                  opts: PollOpt) -> io::Result<()> {
        self.register(poll, token, events, opts)
    }

    fn deregister(&self, _: &Poll) -> io::Result<()> {
        let sigev = SigevNotify::SigevNone;
        self.inner.borrow_mut().set_sigev_notify(sigev);
        Ok(())
    }
}


#[derive(Debug)]
pub struct LioCb<'a> {
    // Unlike AioCb, registering this structure does not modify the AioCb's
    // themselves, so no RefCell is needed
    inner: Box<[aio::AioCb<'a>]>,
    // A plain Cell suffices, because we can Copy SigevNotify's.
    sev: Cell<SigevNotify>
}

impl<'a> LioCb<'a> {
    pub fn aiocb(&mut self, idx: usize) -> &mut aio::AioCb<'a> {
        &mut self.inner[idx]
    }

    pub fn from_boxed_slices(fd: RawFd, offs: off_t, bufs: &[Rc<Box<[u8]>>],
                          prio: c_int, opcode: aio::LioOpcode) -> LioCb<'a>{
        let mut v = Vec::<aio::AioCb>::with_capacity(bufs.len());
        let mut o = offs;
        for x in bufs {
            v.push(
                aio::AioCb::from_boxed_slice(fd, o, x.clone(), prio,
                SigevNotify::SigevNone, opcode));
            o += x.len() as off_t;
        }
        let i = v.into_boxed_slice();
        LioCb {inner:i, sev: Cell::new(SigevNotify::SigevNone)}
    }

    pub fn listio(&mut self) -> nix::Result<()> {
        let aiolist: Vec<&mut nix::sys::aio::AioCb> = Vec::from_iter(self.inner.iter_mut());
        aio::lio_listio(aio::LioMode::LIO_NOWAIT, &aiolist, self.sev.get())
    }
}

impl<'a> Evented for LioCb<'a> {
    fn register(&self,
                poll: &Poll,
                token: Token,
                events: Ready,
                _: PollOpt) -> io::Result<()> {
        assert!(UnixReady::from(events).is_lio());
        let udata = usize::from(token);
        let kq = poll.as_raw_fd();
        let sigev = SigevNotify::SigevKevent{kq: kq, udata: udata as isize};
        self.sev.set(sigev);
        Ok(())
    }

    fn reregister(&self,
                  poll: &Poll,
                  token: Token,
                  events: Ready,
                  opts: PollOpt) -> io::Result<()> {
        self.register(poll, token, events, opts)
    }

    fn deregister(&self, _: &Poll) -> io::Result<()> {
        let sigev = SigevNotify::SigevNone;
        self.sev.set(sigev);
        Ok(())
    }
}
