use actors::Transport;
use futures;
use futures::sync;
use futures::Future;
use futures::Stream;
use net::events::NetworkError;
use net::events::NetworkEvent;
use spnl::codec::FrameCodec;
use spnl::frames::Frame;
use std;
use std::net::SocketAddr;
use std::thread::JoinHandle;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::runtime::TaskExecutor;

/// Default buffer amount for MPSC channels
pub const MPSC_BUF_SIZE: usize = 0;

#[derive(Debug)]
pub enum ConnectionState {
    New,
    Initializing,
    Connected(SocketAddr, sync::mpsc::Sender<Frame>),
    Closed,
    Error(std::io::Error),
}

mod events {
    use std;

    use super::ConnectionState;
    use spnl::frames::Frame;

    /// Network events emitted by the network `Bridge`
    #[derive(Debug)]
    pub enum NetworkEvent {
        Connection(ConnectionState),
        Data(Frame),
    }

    #[derive(Debug)]
    pub enum NetworkError {
        UnsupportedProtocol,
        MissingExecutor,
        Io(std::io::Error),
    }

    impl From<std::io::Error> for NetworkError {
        fn from(other: std::io::Error) -> Self {
            NetworkError::Io(other)
        }
    }
}

/// Bridge to Tokio land, responsible for network connection management
pub struct Bridge {
    /// Executor belonging to the Tokio runtime
    pub(crate) executor: Option<TaskExecutor>,
    /// Queue of network events emitted by the network layer
    events: sync::mpsc::UnboundedSender<NetworkEvent>,
    /// Thread blocking on the Tokio runtime
    net_thread: Option<JoinHandle<()>>,
}

// impl bridge
impl Bridge {
    /// Creates a new [Bridge]
    ///
    /// # Returns
    /// A tuple consisting of the new Bridge object and the network event receiver.
    /// The receiver will allow responding to [NetworkEvent]s for external state management.
    pub fn new() -> (Self, sync::mpsc::UnboundedReceiver<NetworkEvent>) {
        let (sender, receiver) = sync::mpsc::unbounded();
        let bridge = Bridge {
            executor: None,
            events: sender,
            net_thread: None,
        };

        (bridge, receiver)
    }

    /// Starts the Tokio runtime and binds a [TcpListener] to the provided `addr`
    pub fn start(&mut self, addr: SocketAddr) {
        let (ex, th) = start_network_thread("tokio-runtime".into(), addr, self.events.clone());
        self.executor = Some(ex);
        self.net_thread = Some(th);
    }

    /// Attempts to establish a TCP connection to the provided `addr`.
    ///
    /// # Side effects
    /// When the connection is successul:
    ///     - a `ConnectionState::Connected` is dispatched on the network bridge event queue
    ///     - a new task is spawned on the Tokio runtime for driving the TCP connection's  I/O
    ///
    /// # Errors
    /// If the provided protocol is not supported or if the Tokio runtime's executor has not been
    /// set.
    pub fn connect(&mut self, proto: Transport, addr: SocketAddr) -> Result<(), NetworkError> {
        match proto {
            Transport::TCP => {
                println!("connecting to new remote TCP address");
                if let Some(ref executor) = self.executor {
                    let events = self.events.clone();
                    let connect_fut = TcpStream::connect(&addr)
                        .map_err(|_err| {
                            println!("err connecting to remote!");
                            // TODO err
                            ()
                        })
                        .and_then(move |tcp_stream| {
                            let peer_addr = tcp_stream
                                .peer_addr()
                                .expect("stream must have a peer address");
                            let (tx, rx) = sync::mpsc::channel(MPSC_BUF_SIZE);
                            events.unbounded_send(NetworkEvent::Connection(
                                ConnectionState::Connected(peer_addr, tx),
                            ));
                            handle_tcp(tcp_stream, rx, events.clone())
                        });
                    executor.spawn(connect_fut);
                    Ok(())
                } else {
                    Err(NetworkError::MissingExecutor)
                }
            }
            _other => Err(NetworkError::UnsupportedProtocol),
        }
    }
}

/// Spawns a TCP server on the provided `TaskExecutor` and `addr`.
///
/// Connection result and errors are propagated on the provided `events`.
fn start_tcp_server(
    executor: TaskExecutor,
    addr: SocketAddr,
    events: sync::mpsc::UnboundedSender<NetworkEvent>,
) -> impl Future<Item = (), Error = ()> {
    let server = TcpListener::bind(&addr).expect("could not bind to address");
    let err_events = events.clone();
    let server = server
        .incoming()
        .map_err(move |e| {
            println!("err listening on TCP socket");
            err_events.unbounded_send(NetworkEvent::Connection(ConnectionState::Error(e)));
        })
        .for_each(move |tcp_stream| {
            println!("connected TCP client");
            let peer_addr = tcp_stream
                .peer_addr()
                .expect("stream must have a peer address");
            let (tx, rx) = sync::mpsc::channel(MPSC_BUF_SIZE);
            executor.spawn(handle_tcp(tcp_stream, rx, events.clone()));
            events.unbounded_send(NetworkEvent::Connection(ConnectionState::Connected(
                peer_addr, tx,
            )));
            Ok(())
        });
    server
}

/// Spawns a new thread responsible for driving the Tokio runtime to completion.
///
/// # Returns
/// A tuple consisting of the runtime's executor and a handle to the network thread.
fn start_network_thread(
    name: String,
    addr: SocketAddr,
    events: sync::mpsc::UnboundedSender<NetworkEvent>,
) -> (TaskExecutor, JoinHandle<()>) {
    use std::thread::Builder;

    let (tx, rx) = sync::oneshot::channel();
    let th = Builder::new()
        .name(name)
        .spawn(move || {
            let mut runtime = Runtime::new().expect("runtime creation in network main thread");
            let executor = runtime.executor();

            runtime.spawn(start_tcp_server(executor.clone(), addr, events));

            let _res = tx.send(executor);
            runtime.shutdown_on_idle().wait().unwrap();
        })
        .expect("TCP serve thread spawning should not fail");
    let executor = rx
        .wait()
        .expect("could not receive executor clone; did network thread panic?");

    (executor, th)
}

/// Returns a future which drives TCP I/O over the [FrameCodec].
fn handle_tcp(
    stream: TcpStream,
    rx: sync::mpsc::Receiver<Frame>,
    tx: futures::sync::mpsc::UnboundedSender<NetworkEvent>,
) -> impl Future<Item = (), Error = ()> {
    use futures::Stream;

    let transport = FrameCodec::new(stream);
    let (frame_writer, frame_reader) = transport.split();

    frame_reader
        .map(move |frame: Frame| {
            // Handle frame from protocol; yielding `Some(Frame)` if a response `Frame` should be sent
            match frame {
                Frame::StreamRequest(_) => None,
                Frame::CreditUpdate(_) => None,
                Frame::Data(fr) => {
                    // TODO deserialize raw data into MsgEnvelope (if possible), do actor lookup, etc.
                    // TODO handle err
                    tx.unbounded_send(NetworkEvent::Data(Frame::Data(fr)));
                    None
                }
                Frame::Ping(s, id) => {
                    Some(Frame::Pong(s, id))
                }
                Frame::Pong(_, _) => None,
                Frame::Unknown => {
                    None
                }
            }
        })
        .select(
            // Combine above stream with RX side of MPSC channel
            rx
                .map_err(|_err| {
                    () // TODO err
                })
                .map(|frame: Frame| {
                    Some(frame)
                })
        )
        // Forward the combined streams to the frame writer
        .forward(frame_writer)
        .map_err(|_res| {
            // TODO
            ()
        })
        .then(|_| {
            // Connection terminated
            Ok(())
        })
}