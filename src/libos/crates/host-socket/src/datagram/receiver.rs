use std::mem::MaybeUninit;

use io_uring_callback::{Fd, IoHandle};
use sgx_untrusted_alloc::{MaybeUntrusted, UntrustedBox};

use crate::common::Common;
use crate::prelude::*;
use crate::runtime::Runtime;
use core::ffi::c_void;

pub struct Receiver<A: Addr + 'static, R: Runtime> {
    common: Arc<Common<A, R>>,
    inner: Mutex<Inner>,
}

impl<A: Addr, R: Runtime> Receiver<A, R> {
    pub fn new(common: Arc<Common<A, R>>) -> Arc<Self> {
        let inner = Mutex::new(Inner::new());
        Arc::new(Self { common, inner })
    }

    pub async fn recvmsg(
        self: &Arc<Self>,
        bufs: &mut [&mut [u8]],
        flags: RecvFlags,
        mut control: Option<&mut [u8]>,
    ) -> Result<(usize, Option<A>, i32)> {
        let mask = Events::IN;
        // Initialize the poller only when needed
        let mut poller = None;
        loop {
            // Attempt to recv
            let res = self.try_recvmsg(bufs, flags, &mut control);
            if !res.has_errno(EAGAIN) {
                return res;
            }

            // Need more handles for flags not MSG_DONTWAIT
            if self.common.nonblocking()
                || flags.contains(RecvFlags::MSG_DONTWAIT)
                || flags.contains(RecvFlags::MSG_ERRQUEUE)
            {
                return_errno!(EAGAIN, "no data are present to be received");
            }

            if self.is_shutdown() {
                return Ok((0, None, flags.bits()));
            }

            // Wait for interesting events by polling
            if poller.is_none() {
                let new_poller = Poller::new();
                self.common.pollee().connect_poller(mask, &new_poller);
                poller = Some(new_poller);
            }
            let events = self.common.pollee().poll(mask, None);
            if events.is_empty() {
                poller.as_ref().unwrap().wait().await?;
            }
        }
    }

    fn try_recvmsg(
        self: &Arc<Self>,
        bufs: &mut [&mut [u8]],
        flags: RecvFlags,
        control: &mut Option<&mut [u8]>,
    ) -> Result<(usize, Option<A>, i32)> {
        let mut inner = self.inner.lock().unwrap();

        if !flags.is_empty()
            && flags.intersects(
                !(RecvFlags::MSG_DONTWAIT
                    | RecvFlags::MSG_ERRQUEUE
                    | RecvFlags::MSG_TRUNC
                    | RecvFlags::MSG_PEEK),
            )
        {
            // todo!("Support other flags");
            return_errno!(EINVAL, "the socket flags is not supported");
        }

        // Mark the socket as non-readable since Datagram uses single packet
        self.common.pollee().del_events(Events::IN);

        // Copy data from the recv buffer to the bufs
        let copied_bytes = inner.try_copy_buf(bufs);
        if let Some(copied_bytes) = copied_bytes {
            let recv_addr = inner.get_packet_addr();
            // Copy ancillary data from control buffer
            if inner.req.msg.msg_controllen > 0 {
                control
                    .as_mut()
                    .map(|buf| buf.copy_from_slice(&inner.msg_control[..buf.len()]));
            }

            let bufs_len: usize = bufs.iter().map(|buf| buf.len()).sum();
            let msg_flags = if bufs_len < inner.recv_len().unwrap() {
                RecvFlags::MSG_TRUNC
            } else {
                flags
            }
            .bits();

            let recv_bytes = if flags.contains(RecvFlags::MSG_TRUNC) {
                inner.recv_len().unwrap()
            } else {
                copied_bytes
            };

            // When flags contain MSG_PEEK and there is data in socket recv buffer, it is unnecessary to
            // send blocking recv request (do_recv) to fetch data from iouring buffer, which may flush the data in recv buffer.
            // When flags don't contain MSG_PEEK or there is no available data, it is time to send blocking request to iouring for notifying events.
            if !flags.contains(RecvFlags::MSG_PEEK) {
                self.do_recv(&mut inner);
            }

            return Ok((recv_bytes, recv_addr, msg_flags));
        }

        if let Some(errno) = inner.error {
            self.do_recv(&mut inner);
            return_errno!(errno, "recv failed");
        }

        if inner.is_shutdown {
            return_errno!(Errno::EWOULDBLOCK, "the socket recv has been shutdown");
        }

        self.do_recv(&mut inner);
        return_errno!(EAGAIN, "try recv again");
    }

    fn do_recv(self: &Arc<Self>, inner: &mut MutexGuard<Inner>) {
        if inner.io_handle.is_some() || self.common.is_closed() {
            return;
        }
        // Clear recv_len and error
        inner.recv_len.take();
        inner.error.take();

        if inner.is_shutdown {
            info!("do_recv early return, the socket recv has been shutdown");
            return;
        }

        let receiver = self.clone();
        // Init the callback invoked upon the completion of the async recv
        let complete_fn = move |retval: i32| {
            let mut inner = receiver.inner.lock().unwrap();

            // Release the handle to the async recv
            inner.io_handle.take();

            // Handle error
            if retval < 0 {
                // TODO: Should we filter the error case? Do we have the ability to filter?
                // We only filter the normal case now. According to the man page of recvmsg,
                // these errors should not happen, since our fd and arguments should always
                // be valid unless being attacked.

                // TODO: guard against Iago attack through errno
                let errno = Errno::from(-retval as u32);
                inner.error = Some(errno);
                // TODO: add PRI event if set SO_SELECT_ERR_QUEUE
                receiver.common.pollee().add_events(Events::ERR);
                return;
            }

            // Handle the normal case of a successful read
            inner.recv_len = Some(retval as usize);
            receiver.common.pollee().add_events(Events::IN);

            // We don't do_recv() here, since do_recv() will clear the recv message.
        };

        // Generate the async recv request
        let msghdr_ptr = inner.new_recv_req();

        // Submit the async recv to io_uring
        let io_uring = self.common.io_uring();
        let host_fd = Fd(self.common.host_fd() as _);
        let handle = unsafe { io_uring.recvmsg(host_fd, msghdr_ptr, 0, complete_fn) };
        inner.io_handle.replace(handle);
    }

    pub fn initiate_async_recv(self: &Arc<Self>) {
        let mut inner = self.inner.lock().unwrap();
        self.do_recv(&mut inner);
    }

    pub fn cancel_requests(&self) {
        let inner = self.inner.lock().unwrap();
        if let Some(io_handle) = &inner.io_handle {
            let io_uring = self.common.io_uring();
            unsafe { io_uring.cancel(io_handle) };
        }
    }

    /// Shutdown udp receiver.
    pub fn shutdown(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.is_shutdown = true;
    }

    /// Reset udp receiver shutdown state.
    pub fn reset_shutdown(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.is_shutdown = false;
    }

    /// Obtain udp receiver shutdown state.
    fn is_shutdown(&self) -> bool {
        self.inner.lock().unwrap().is_shutdown
    }

    pub fn ready_len(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.recv_len().unwrap_or(0)
    }
}

struct Inner {
    recv_buf: UntrustedBox<[u8]>,
    // Datagram sockets in various domains permit zero-length datagrams.
    // Hence the recv_len might be 0.
    recv_len: Option<usize>,
    msg_control: UntrustedBox<[u8]>,
    req: UntrustedBox<RecvReq>,
    io_handle: Option<IoHandle>,
    error: Option<Errno>,
    is_shutdown: bool,
}

unsafe impl Send for Inner {}

impl Inner {
    pub fn new() -> Self {
        Self {
            recv_buf: UntrustedBox::new_uninit_slice(super::MAX_BUF_SIZE),
            recv_len: None,
            msg_control: UntrustedBox::new_uninit_slice(super::OPTMEM_MAX),
            req: UntrustedBox::new_uninit(),
            io_handle: None,
            error: None,
            is_shutdown: false,
        }
    }

    pub fn new_recv_req(&mut self) -> *mut libc::msghdr {
        let iovec = libc::iovec {
            iov_base: self.recv_buf.as_mut_ptr() as _,
            iov_len: self.recv_buf.len(),
        };

        let msghdr_ptr = &raw mut self.req.msg;

        let mut msg: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
        msg.msg_iov = &raw mut self.req.iovec as _;
        msg.msg_iovlen = 1;
        msg.msg_name = &raw mut self.req.addr as _;
        msg.msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as _;

        self.req.control = self.msg_control.as_mut_ptr() as _;
        msg.msg_control = self.req.control as _;
        msg.msg_controllen = self.msg_control.len() as _;

        self.req.msg = msg;
        self.req.iovec = iovec;

        msghdr_ptr
    }

    pub fn try_copy_buf(&self, bufs: &mut [&mut [u8]]) -> Option<usize> {
        self.recv_len.map(|recv_len| {
            let mut copy_len = 0;
            for buf in bufs {
                let recv_buf = &self.recv_buf[copy_len..recv_len];
                if buf.len() <= recv_buf.len() {
                    buf.copy_from_slice(&recv_buf[..buf.len()]);
                    copy_len += buf.len();
                } else {
                    buf[..recv_buf.len()].copy_from_slice(&recv_buf[..]);
                    copy_len += recv_buf.len();
                    break;
                }
            }
            copy_len
        })
    }

    pub fn recv_len(&self) -> Option<usize> {
        self.recv_len
    }

    /// Return the addr of the received packet if udp socket is not connected.
    /// Return None if udp socket is connected.
    pub fn get_packet_addr<A: Addr>(&self) -> Option<A> {
        let recv_addr_len = self.req.msg.msg_namelen as usize;
        A::from_c_storage(&self.req.addr, recv_addr_len).ok()
    }
}

#[repr(C)]
struct RecvReq {
    msg: libc::msghdr,
    iovec: libc::iovec,
    addr: libc::sockaddr_storage,
    control: *mut c_void,
}

unsafe impl MaybeUntrusted for RecvReq {}
