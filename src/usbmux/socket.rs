use std::io;
use std::net::TcpStream;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// Object-safe Read+Write+Send — used for the External variant.
pub trait RwStream: io::Read + io::Write + Send {}
impl<T: io::Read + io::Write + Send> RwStream for T {}

/// Platform-agnostic read/write wrapper.
/// On macOS/Linux: Unix domain socket to /var/run/usbmuxd.
/// On Windows:     TCP connection to 127.0.0.1:27015.
/// External:       Caller-provided stream (e.g. a Wasm WIT conn resource).
pub enum MuxSocket {
    #[cfg(unix)]
    Unix(UnixStream),
    Tcp(TcpStream),
    External(Box<dyn RwStream>),
}

impl MuxSocket {
    pub fn connect() -> io::Result<Self> {
        // Allow override for testing or containerised environments
        if let Ok(path) = std::env::var("USBMUXD_SOCKET_ADDRESS") {
            if let Some((host, port)) = path.split_once(':') {
                let port: u16 = port.parse().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "bad USBMUXD_SOCKET_ADDRESS port",
                    )
                })?;
                return Ok(MuxSocket::Tcp(TcpStream::connect((host, port))?));
            }
            #[cfg(unix)]
            return Ok(MuxSocket::Unix(UnixStream::connect(&path)?));
        }

        #[cfg(unix)]
        {
            // macOS and Linux default
            if let Ok(s) = UnixStream::connect("/var/run/usbmuxd") {
                return Ok(MuxSocket::Unix(s));
            }
            // Homebrew usbmuxd path
            if let Ok(s) = UnixStream::connect("/tmp/usbmuxd") {
                return Ok(MuxSocket::Unix(s));
            }
        }

        // Windows (iTunes), Docker host proxy, or any socat/tcp relay on 27015
        Ok(MuxSocket::Tcp(TcpStream::connect("127.0.0.1:27015")?))
    }

    /// Wrap a caller-provided stream (e.g. a Wasm WIT conn resource).
    pub fn external<S: RwStream + 'static>(stream: S) -> Self {
        MuxSocket::External(Box::new(stream))
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(unix)]
            MuxSocket::Unix(s) => Ok(MuxSocket::Unix(s.try_clone()?)),
            MuxSocket::Tcp(s) => Ok(MuxSocket::Tcp(s.try_clone()?)),
            MuxSocket::External(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "External MuxSocket cannot be cloned",
            )),
        }
    }
}

impl io::Read for MuxSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            MuxSocket::Unix(s) => s.read(buf),
            MuxSocket::Tcp(s) => s.read(buf),
            MuxSocket::External(s) => s.read(buf),
        }
    }
}

impl io::Write for MuxSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            MuxSocket::Unix(s) => s.write(buf),
            MuxSocket::Tcp(s) => s.write(buf),
            MuxSocket::External(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            MuxSocket::Unix(s) => s.flush(),
            MuxSocket::Tcp(s) => s.flush(),
            MuxSocket::External(s) => s.flush(),
        }
    }
}
