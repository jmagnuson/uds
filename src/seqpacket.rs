use std::io::{self, ErrorKind, IoSlice, IoSliceMut};
use std::mem;
use std::os::unix::io::{RawFd, FromRawFd, AsRawFd, IntoRawFd};
use std::path::Path;

use libc::{SOCK_SEQPACKET, MSG_EOR, c_void, close, send};

#[cfg(feature="mio")]
use mio::{event::Evented, unix::EventedFd, Ready, Poll, PollOpt, Token};

use crate::addr::*;
use crate::helpers::*;
use crate::ancillary::*;

/// Implement traits apropriate for any file-descriptor-wrapping type.
macro_rules! impl_rawfd_traits {($type:tt) => {
    impl FromRawFd for $type {
        unsafe fn from_raw_fd(fd: RawFd) -> Self {
            $type { fd }
        }
    }
    impl AsRawFd for $type {
        fn as_raw_fd(&self) -> RawFd {
            self.fd
        }
    }
    impl IntoRawFd for $type {
        fn into_raw_fd(self) -> RawFd {
            let fd = self.fd;
            mem::forget(self);
            fd
        }
    }
    impl Drop for $type {
        fn drop(&mut self) {
            let _ = unsafe { close(self.fd) };
        }
    }
    #[cfg(feature="mio")]
    impl Evented for $type {
        fn register(&self,  poll: &Poll,  token: Token,  interest: Ready,  opts: PollOpt)
        -> Result<(), io::Error> {
            EventedFd(&self.fd).register(poll, token, interest, opts)
        }

        fn reregister(&self,  poll: &Poll,  token: Token,  interest: Ready,  opts: PollOpt)
        -> Result<(), io::Error> {
            EventedFd(&self.fd).reregister(poll, token, interest, opts)
        }

        fn deregister(&self,  poll: &Poll) -> Result<(), io::Error> {
            EventedFd(&self.fd).deregister(poll)
        }
    }
}}



/// An unix sequential packet connection.
#[derive(Debug)]
#[repr(transparent)]
pub struct UnixSeqpacketConn {
    fd: RawFd,
}

impl_rawfd_traits!{UnixSeqpacketConn}

impl UnixSeqpacketConn {
    /// Connect to an unix seqpacket server listening at `path`.
    ///
    /// This is a wrapper around [`connect_unix_addr()`](#method.connect_unix_addr)
    /// for convenience and compatibility with std.
    pub fn connect<P: AsRef<Path>>(path: P) -> Result<Self, io::Error> {
        let addr = UnixSocketAddr::from_path(&path)?;
        Self::connect_unix_addr(&addr)
    }
    /// Connect to an unix seqpacket server listening at `addr`.
    pub fn connect_unix_addr(addr: &UnixSocketAddr) -> Result<Self, io::Error> {
        let socket = Socket::new(SOCK_SEQPACKET, false)?;
        connect_to(socket.as_raw_fd(), addr)?;
        Ok(UnixSeqpacketConn { fd: socket.into_raw_fd() })
    }
    /// Bind to an address before connecting to a listening sequplacet socket.
    pub fn connect_from_to_unix_addr(from: &UnixSocketAddr,  to: &UnixSocketAddr)
    -> Result<Self, io::Error> {
        let socket = Socket::new(SOCK_SEQPACKET, false)?;
        bind_to(socket.as_raw_fd(), from)?;
        connect_to(socket.as_raw_fd(), to)?;
        Ok(UnixSeqpacketConn { fd: socket.into_raw_fd() })
    }
    /// Create a pair of unix-domain seqpacket conneections connected to each other.
    ///
    /// # Examples
    ///
    /// Both sides have the unnamed address.
    /// ```
    /// # use uds::UnixSeqpacketConn;
    /// let (a, b) = UnixSeqpacketConn::pair().unwrap();
    /// assert!(a.local_unix_addr().unwrap().is_unnamed());
    /// assert!(b.local_unix_addr().unwrap().is_unnamed());
    /// ```
    ///
    /// Send & receive a packet:
    /// ```
    /// # use uds::UnixSeqpacketConn;
    /// let (a, b) = UnixSeqpacketConn::pair().unwrap();
    /// a.send(b"hello").unwrap();
    /// assert!(b.recv(&mut[0; 20]).unwrap().1);
    /// ```
    pub fn pair() -> Result<(Self, Self), io::Error> {
        let (a, b) = Socket::pair(SOCK_SEQPACKET, false)?;
        let a = UnixSeqpacketConn { fd: a.into_raw_fd() };
        let b = UnixSeqpacketConn { fd: b.into_raw_fd() };
        Ok((a, b))
    }
    
    /// Get the address of this side of the connection.
    pub fn local_unix_addr(&self) -> Result<UnixSocketAddr, io::Error> {
        local_addr(self.fd)
    }
    /// Get the address of the other side of the connection.
    pub fn peer_unix_addr(&self) -> Result<UnixSocketAddr, io::Error> {
        peer_addr(self.fd)
    }

    /// Send a packet to the peer.
    pub fn send(&self,  packet: &[u8]) -> Result<usize, io::Error> {
        let ptr = packet.as_ptr() as *const c_void;
        let flags = MSG_NOSIGNAL | MSG_EOR;
        let sent = cvt_r!(unsafe { send(self.fd, ptr, packet.len(), flags) })?;
        Ok(sent as usize)
    }
    /// Receive a packet or parts of one from the peer.
    ///
    /// The returned `bool` indicates whether the received bytes completed a
    /// packet.
    pub fn recv(&self,  buffer: &mut[u8]) -> Result<(usize, bool), io::Error> {
        let mut buffers = [IoSliceMut::new(buffer)];
        let (bytes, ancillary) = recv_ancillary(self.fd, None, 0, &mut buffers, &mut[])?;
        Ok((bytes, ancillary.message_truncated()))
    }
    /// Send a packet assembled from multiple byte slices.
    pub fn send_vectored(&self,  slices: &[IoSlice])
    -> Result<usize, io::Error> {
        // Can't use writev() because we need to pass flags,
        // and the flags accepted by pwritev2() aren't the one we need to pass.
        send_ancillary(self.as_raw_fd(), None, MSG_EOR, slices, &[], None)
    }
    /// Read (part of) a packet into multiple buffers.
    ///
    /// The returned `bool` indicates whether the received bytes completed a
    /// packet.
    pub fn recv_vectored(&self,  buffers: &mut[IoSliceMut])
    -> Result<(usize, bool), io::Error> {
        recv_ancillary(self.fd, None, 0, buffers, &mut[])
            .map(|(bytes, ancillary)| (bytes, ancillary.message_truncated()) )
    }

    /// Create a new file descriptor also pointing to this side of this connection.
    pub fn try_clone(&self) -> Result<Self, io::Error> {
        let cloned = Socket::try_clone_from(self.fd)?;
        Ok(UnixSeqpacketConn { fd: cloned.into_raw_fd() })
    }

    /// Enable or disable nonblocking mode.
    ///
    /// Consider using the nonblocking variant of this type instead.
    /// This method mainly exists for feature parity with std's `UnixStream`.
    ///
    /// # Examples
    ///
    /// Trying to receive when there are no packets waiting:
    ///
    /// ```
    /// # use std::io::ErrorKind;
    /// # use uds::UnixSeqpacketConn;
    /// let (a, b) = UnixSeqpacketConn::pair().expect("create seqpacket pair");
    /// a.set_nonblocking(true).unwrap();
    /// assert_eq!(a.recv(&mut[]).unwrap_err().kind(), ErrorKind::WouldBlock);
    /// ```
    ///
    /// Trying to send when the OS buffer for the connection is full:
    ///
    /// ```
    /// # use std::io::ErrorKind;
    /// # use uds::UnixSeqpacketConn;
    /// let (a, b) = UnixSeqpacketConn::pair().expect("create seqpacket pair");
    /// a.set_nonblocking(true).unwrap();
    /// loop {
    ///     if let Err(error) = a.send(&[b'#'; 1000]) {
    ///         assert_eq!(error.kind(), ErrorKind::WouldBlock);
    ///         break;
    ///     }
    /// }
    /// ```
    pub fn set_nonblocking(&self,  nonblocking: bool) -> Result<(), io::Error> {
        set_nonblocking(self.fd, nonblocking)
    }
}



/// An unix domain listener sequential-packet connections.
#[derive(Debug)]
#[repr(transparent)]
pub struct UnixSeqpacketListener {
    fd: RawFd
}
impl_rawfd_traits!{UnixSeqpacketListener}
impl UnixSeqpacketListener {
    pub fn bind<P: AsRef<Path>>(path: P) -> Result<Self, io::Error> {
        let addr = UnixSocketAddr::from_path(path.as_ref())?;
        Self::bind_unix_addr(&addr)
    }
    pub fn bind_unix_addr(addr: &UnixSocketAddr) -> Result<Self, io::Error> {
        let socket = Socket::new(SOCK_SEQPACKET, false)?;
        bind_to(socket.as_raw_fd(), addr)?;
        socket.start_listening()?;
        Ok(unsafe { Self::from_raw_fd(socket.into_raw_fd()) })
    }

    pub fn local_unix_addr(&self) -> Result<UnixSocketAddr, io::Error> {
        local_addr(self.fd)
    }

    pub fn accept_unix_addr(&self)
    -> Result<(UnixSeqpacketConn, UnixSocketAddr), io::Error> {
        let (socket, addr) = Socket::accept_from(self.as_raw_fd(), false)?;
        let conn = UnixSeqpacketConn { fd: socket.into_raw_fd() };
        Ok((conn, addr))
    }

    /// Create a new file descriptor listening for the same connections.
    pub fn try_clone(&self) -> Result<Self, io::Error> {
        let cloned = Socket::try_clone_from(self.fd)?;
        Ok(UnixSeqpacketListener { fd: cloned.into_raw_fd() })
    }
    /// Enable or disable nonblocking-ness of [`accept_unix_addr()`](#method.accept_unix addr).
    ///
    /// The returned connnections will still be in blocking mode regardsless.
    ///
    /// Consider using the nonblocking variant of this type instead;
    /// this method mostly exists for feature parity with std's `UnixListener`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::io::ErrorKind;
    /// # use uds::{UnixSocketAddr, UnixSeqpacketListener};
    /// #
    /// # let addr = UnixSocketAddr::new_unspecified();
    /// let listener = UnixSeqpacketListener::bind_unix_addr(&addr).unwrap();
    /// listener.set_nonblocking(true).unwrap();
    /// assert_eq!(listener.accept_unix_addr().unwrap_err().kind(), ErrorKind::WouldBlock);
    /// ```
    pub fn set_nonblocking(&self,  nonblocking: bool) -> Result<(), io::Error> {
        set_nonblocking(self.fd, nonblocking)
    }
}



/// A non-blocking unix domain sequential-packet connection.
///
/// Differs from [`UnixSeqpacketConn`](../struct.UnixSeqpacketConn.html)
/// in that all operations that send or receive data will return an `Error` of
/// kind `ErrorKind::WouldBlock` instead of blocking.
/// This is done by creating the socket as non-blocking, and not by passing
/// `MSG_DONTWAIT`. If creating this type from a raw file descriptor, ensure
/// the fd is set to nonblocking before using it through this type.
///
/// This type can be used with mio if the `mio` feature is enabled:
/// 
/// ```toml
/// uds = { version = "x.y", features=["mio"] }
/// ```
#[derive(Debug)]
#[repr(transparent)]
pub struct NonblockingUnixSeqpacketConn {
    fd: RawFd,
}

impl_rawfd_traits!{NonblockingUnixSeqpacketConn}

// can't Deref<Target=UnixSeqpacketConn> because that would include try_clone()
impl NonblockingUnixSeqpacketConn {
    /// Create a new file descriptor also pointing to this side of this connection.
    pub fn try_clone(&self) -> Result<Self, io::Error> {
        let cloned = Socket::try_clone_from(self.fd)?;
        // nonblockingness is shared and therefore inherited
        Ok(NonblockingUnixSeqpacketConn { fd: cloned.into_raw_fd() })
    }
}



/// A non-blocking unix domain listener for sequential-packet connections.
///
/// Differs from [`UnixSeqpacketListener`](../struct.UnixSeqpacketListener.html)
/// in that [`accept()`](struct.NonblockingUnixSeqpacketListener.html#method.accept)
/// returns non-blocking [connection sockets](struct.NonblockingUnixSeqpacketConn.html)
/// and doesn't block if no client `connect()`ions are pending.
///
/// This type can be used with mio if the `mio` feature is enabled:
/// 
/// ```toml
/// uds = { version = "x.y", features=["mio"] }
/// ```
#[derive(Debug)]
#[repr(transparent)]
pub struct NonblockingUnixSeqpacketListener {
    fd: RawFd
}

impl_rawfd_traits!{NonblockingUnixSeqpacketListener}

impl NonblockingUnixSeqpacketListener {
    pub fn bind<P: AsRef<Path>>(path: P) -> Result<Self, io::Error> {
        let addr = UnixSocketAddr::from_path(&path)?;
        Self::bind_unix_addr(&addr)
    }
    pub fn bind_unix_addr(addr: &UnixSocketAddr) -> Result<Self, io::Error> {
        let socket = Socket::new(SOCK_SEQPACKET, true)?;
        bind_to(socket.as_raw_fd(), addr)?;
        socket.start_listening()?;
        Ok(NonblockingUnixSeqpacketListener { fd: socket.into_raw_fd() })
    }
    /// Get the address this listener was bound to.
    pub fn local_unix_addr(&self) -> Result<UnixSocketAddr, io::Error> {
        local_addr(self.fd)
    }
    /// Accept a non-blocking connection, non-blockingly.
    pub fn accept_unix_addr(&self)
    -> Result<(NonblockingUnixSeqpacketConn, UnixSocketAddr), io::Error> {
        let (socket, addr) = Socket::accept_from(self.fd, true)?;
        let conn = NonblockingUnixSeqpacketConn { fd: socket.into_raw_fd() };
        Ok((conn, addr))
    }

    /// Create a new file descriptor listening for the same connections.
    pub fn try_clone(&self) -> Result<Self, io::Error> {
        let cloned = Socket::try_clone_from(self.fd)?;
        // nonblockingness is shared and therefore inherited
        Ok(NonblockingUnixSeqpacketListener { fd: cloned.into_raw_fd() })
    }
}
