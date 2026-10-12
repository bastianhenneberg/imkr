//! Passing open file descriptors to another process over a Unix socket
//! (SCM_RIGHTS), for moving a window's ptys to another session.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// The most descriptors one message may carry on Linux.
pub const MAX_FDS: usize = 253;

/// Space for the control message carrying `count` descriptors.
fn space(count: usize) -> usize {
    // SAFETY: a pure size calculation.
    unsafe { libc::CMSG_SPACE((count * size_of::<RawFd>()) as u32) as usize }
}

/// Sends `fds` along with one byte. The receiver gets its own copies.
pub fn send(stream: &UnixStream, fds: &[RawFd]) -> io::Result<()> {
    if fds.len() > MAX_FDS {
        return Err(io::Error::other("too many panes to move at once"));
    }
    let mut byte = [0u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = vec![0u8; space(fds.len())];
    // SAFETY: msghdr is plain data, zeroed is a valid empty header.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len() as _;
    // SAFETY: the buffer is sized by CMSG_SPACE for exactly these
    // descriptors, so the first header and its data fit.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(size_of_val(fds) as u32) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg).cast(), fds.len());
    }
    // SAFETY: msg points at live buffers for the whole call.
    let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Receives the byte and descriptors `send` sent, at most `count` of them.
pub fn recv(stream: &UnixStream, count: usize) -> io::Result<Vec<OwnedFd>> {
    let count = count.min(MAX_FDS);
    let mut byte = [0u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = vec![0u8; space(count.max(1))];
    // SAFETY: as in `send`.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len() as _;
    // Received descriptors are closed on exec like everything else here.
    // SAFETY: msg points at live buffers for the whole call.
    let got = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    if got == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let mut fds = Vec::new();
    // SAFETY: walking the control messages the kernel filled in, each
    // SCM_RIGHTS one holding descriptors that are now ours.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                let bytes = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                for i in 0..bytes / size_of::<RawFd>() {
                    fds.push(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::other("some descriptors did not arrive"));
    }
    Ok(fds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{Read, Write};

    #[test]
    fn descriptors_arrive_and_work() {
        let (a, b) = UnixStream::pair().unwrap();
        let (mut pipe_read, pipe_write) = {
            let mut fds = [0; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
            unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
        };
        send(&a, &[pipe_write.as_raw_fd()]).unwrap();
        drop(pipe_write);
        let fds = recv(&b, 4).unwrap();
        assert_eq!(fds.len(), 1);
        let mut copy = File::from(fds.into_iter().next().unwrap());
        copy.write_all(b"hi").unwrap();
        drop(copy);
        let mut text = String::new();
        pipe_read.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hi");
    }
}
