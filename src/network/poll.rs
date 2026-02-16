use libc::{
    epoll_create1,
    epoll_ctl,
    epoll_wait,
    epoll_event,
    EPOLL_CTL_ADD,
    EPOLL_CTL_DEL,
    EPOLLIN,
    EPOLLOUT,
    EPOLLET,
};
use std::os::unix::io::RawFd;
use std::io;

pub struct Poller {
    epoll_fd: RawFd,
}

impl Poller {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { epoll_create1(0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { epoll_fd: fd })
    }

    pub fn add(&self, fd: RawFd, events: u32) -> io::Result<()> {
        let mut event = epoll_event { events, u64: fd as u64 };
        let res = unsafe { epoll_ctl(self.epoll_fd, EPOLL_CTL_ADD, fd, &mut event) };
        if res < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn modify(&self, fd: RawFd, events: u32) -> io::Result<()> {
        let mut event = epoll_event { events, u64: fd as u64 };
        let res = unsafe { epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_MOD, fd, &mut event) };
        if res < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn delete(&self, fd: RawFd) -> io::Result<()> {
        let res = unsafe { epoll_ctl(self.epoll_fd, EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
        if res < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn wait(&self, events: &mut [epoll_event], timeout: i32) -> io::Result<usize> {
        let res = unsafe {
            epoll_wait(self.epoll_fd, events.as_mut_ptr(), events.len() as i32, timeout)
        };
        if res < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(res as usize)
        }
    }
}
