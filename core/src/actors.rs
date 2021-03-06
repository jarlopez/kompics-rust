use bytes::{Buf, IntoBuf};
use crossbeam::sync::MsQueue;
use std::any::Any;
use std::convert::TryFrom;
use std::error::Error;
use std::fmt;
use std::fmt::Debug;
use std::net::{AddrParseError, IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Weak;
use uuid::Uuid;

use super::*;
use messaging::CastEnvelope;
use messaging::DispatchEnvelope;
use messaging::MsgEnvelope;
use messaging::PathResolvable;
use messaging::ReceiveEnvelope;
use std::sync::Arc;

pub trait ActorRaw: ExecuteSend {
    fn receive(&mut self, env: ReceiveEnvelope) -> ();
}

pub trait Dispatcher: ExecuteSend {
    fn receive(&mut self, env: DispatchEnvelope) -> ();
    fn system_path(&mut self) -> SystemPath;
}

pub trait Actor {
    fn receive_local(&mut self, sender: ActorRef, msg: Box<Any>) -> ();
    fn receive_message(&mut self, sender: ActorPath, ser_id: u64, buf: &mut Buf) -> ();
}

impl<CD> ActorRaw for CD
where
    CD: Actor,
{
    fn receive(&mut self, env: ReceiveEnvelope) -> () {
        match env {
            ReceiveEnvelope::Cast(c) => self.receive_local(c.src, c.v),
            ReceiveEnvelope::Msg {
                src,
                dst: _,
                ser_id,
                data,
            } => self.receive_message(src, ser_id, &mut data.into_buf()),
        }
    }
}

pub trait ActorRefFactory {
    fn actor_ref(&self) -> ActorRef;
}

pub trait Dispatching {
    fn dispatcher_ref(&self) -> ActorRef;
}

#[derive(Clone)]
pub struct ActorRef {
    component: Weak<CoreContainer>,
    msg_queue: Weak<MsQueue<MsgEnvelope>>,
}

impl ActorRef {
    pub(crate) fn new(
        component: Weak<CoreContainer>,
        msg_queue: Weak<MsQueue<MsgEnvelope>>,
    ) -> ActorRef {
        ActorRef {
            component,
            msg_queue,
        }
    }

    pub(crate) fn enqueue(&self, env: MsgEnvelope) -> () {
        match (self.msg_queue.upgrade(), self.component.upgrade()) {
            (Some(q), Some(c)) => {
                q.push(env);
                match c.core().increment_work() {
                    SchedulingDecision::Schedule => {
                        let system = c.core().system();
                        system.schedule(c.clone());
                    }
                    _ => (), // nothing
                }
            }
            (q, c) => println!(
                "Dropping msg as target (queue? {:?}, component? {:?}) is unavailable: {:?}",
                q.is_some(),
                c.is_some(),
                env
            ),
        }
    }

    pub fn tell<T, S>(&self, v: Box<T>, from: &S) -> ()
    where
        T: Send + 'static,
        S: ActorRefFactory,
    {
        let bany: Box<Any + Send> = v as Box<Any + Send>;
        let env = DispatchEnvelope::Cast(CastEnvelope {
            src: from.actor_ref(),
            v: bany,
        });
        self.enqueue(MsgEnvelope::Dispatch(env))
    }

    // TODO figure out a way to have one function match both cases -.-
    pub fn tell_any<S>(&self, v: Box<Any + Send>, from: &S) -> ()
    where
        S: ActorRefFactory,
    {
        let env = DispatchEnvelope::Cast(CastEnvelope {
            src: from.actor_ref(),
            v,
        });
        self.enqueue(MsgEnvelope::Dispatch(env))
    }

    /// Attempts to upgrade the contained component, returning `true` if possible.
    pub(crate) fn can_upgrade_component(&self) -> bool {
        self.component.upgrade().is_some()
    }
}

impl ActorRefFactory for ActorRef {
    fn actor_ref(&self) -> ActorRef {
        self.clone()
    }
}

impl Debug for ActorRef {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "<actor-ref>")
    }
}

impl fmt::Display for ActorRef {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "<actor-ref>")
    }
}

impl PartialEq for ActorRef {
    fn eq(&self, other: &ActorRef) -> bool {
        match (self.component.upgrade(), other.component.upgrade()) {
            (Some(ref me), Some(ref it)) => Arc::ptr_eq(me, it),
            _ => false,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(u8)]
pub enum Transport {
    LOCAL = 0b00,
    TCP = 0b01,
    UDP = 0b10,
}

// impl Transport
impl Transport {
    pub fn is_local(&self) -> bool {
        match *self {
            Transport::LOCAL => true,
            _ => false,
        }
    }

    pub fn is_remote(&self) -> bool {
        !self.is_local()
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &Transport::LOCAL => write!(fmt, "local"),
            &Transport::TCP => write!(fmt, "tcp"),
            &Transport::UDP => write!(fmt, "udp"),
        }
    }
}

impl FromStr for Transport {
    type Err = TransportParseError;
    fn from_str(s: &str) -> Result<Transport, TransportParseError> {
        match s {
            "local" => Ok(Transport::LOCAL),
            "tcp" => Ok(Transport::TCP),
            "udp" => Ok(Transport::UDP),
            _ => Err(TransportParseError(())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransportParseError(());
impl fmt::Display for TransportParseError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.write_str(self.description())
    }
}
impl Error for TransportParseError {
    fn description(&self) -> &str {
        "Transport must be one of [local,tcp,udp]"
    }
}

#[derive(Clone, Debug)]
pub enum PathParseError {
    Form(String),
    Transport(TransportParseError),
    Addr(AddrParseError),
}
impl fmt::Display for PathParseError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.write_str(self.description())
    }
}
impl Error for PathParseError {
    fn description(&self) -> &str {
        "Path could not be parsed"
    }
    fn cause(&self) -> Option<&Error> {
        match self {
            &PathParseError::Form(_) => None,
            &PathParseError::Transport(ref e) => Some(e),
            &PathParseError::Addr(ref e) => Some(e),
        }
    }
}
impl From<TransportParseError> for PathParseError {
    fn from(e: TransportParseError) -> PathParseError {
        PathParseError::Transport(e)
    }
}
impl From<AddrParseError> for PathParseError {
    fn from(e: AddrParseError) -> PathParseError {
        PathParseError::Addr(e)
    }
}

#[derive(Clone, Debug)]
pub struct SystemPath {
    protocol: Transport,
    // TODO address could be IPv4, IPv6, or a domain name (not supported yet)
    address: IpAddr,
    port: u16,
}

impl SystemPath {
    pub fn new(protocol: Transport, address: IpAddr, port: u16) -> SystemPath {
        SystemPath {
            protocol,
            address,
            port,
        }
    }

    pub fn with_socket(protocol: Transport, socket: SocketAddr) -> SystemPath {
        SystemPath {
            protocol,
            address: socket.ip(),
            port: socket.port(),
        }
    }

    pub fn address(&self) -> &IpAddr {
        &self.address
    }
}

impl fmt::Display for SystemPath {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "{}://{}:{}", self.protocol, self.address, self.port)
    }
}

pub(crate) trait SystemField {
    fn system(&self) -> &SystemPath;

    fn protocol(&self) -> Transport {
        self.system().protocol
    }
    fn address(&self) -> &IpAddr {
        &self.system().address
    }
    fn port(&self) -> u16 {
        self.system().port
    }
}

impl SystemField for SystemPath {
    fn system(&self) -> &SystemPath {
        self
    }
}

pub trait ActorSource: Dispatching {
    fn path_resolvable(&self) -> PathResolvable;
}

#[derive(Clone, Debug)]
#[repr(u8)]
pub enum ActorPath {
    Unique(UniquePath),
    Named(NamedPath),
}

impl ActorPath {
    pub fn tell<S, B>(&self, m: B, from: &S) -> ()
    where
        S: ActorSource,
        B: Into<Box<Serialisable>>,
    {
        let msg: Box<Serialisable> = m.into();
        let src = from.path_resolvable();
        let dst = self.clone();
        let env = DispatchEnvelope::Msg { src, dst, msg };
        from.dispatcher_ref().enqueue(MsgEnvelope::Dispatch(env))
    }
}

impl SystemField for ActorPath {
    fn system(&self) -> &SystemPath {
        match self {
            &ActorPath::Unique(ref up) => up.system(),
            &ActorPath::Named(ref np) => np.system(),
        }
    }
}

impl From<(SystemPath, Uuid)> for ActorPath {
    fn from(t: (SystemPath, Uuid)) -> ActorPath {
        ActorPath::Unique(UniquePath {
            system: t.0,
            id: t.1,
        })
    }
}

const PATH_SEP: &'static str = "/";

impl fmt::Display for ActorPath {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &ActorPath::Named(ref np) => {
                let path = np
                    .path
                    .iter()
                    .fold(String::new(), |acc, arg| acc + PATH_SEP + arg);
                write!(fmt, "{}{}", np.system, path)
            }
            &ActorPath::Unique(ref up) => write!(fmt, "{}{}{}", up.system, PATH_SEP, up.id),
        }
    }
}

impl TryFrom<String> for ActorPath {
    type Error = PathParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let p = ActorPath::from_str(&s)?;
        Ok(p)
    }
}

impl FromStr for ActorPath {
    type Err = PathParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // TODO UniquePath
        let p = NamedPath::from_str(s)?;
        Ok(ActorPath::Named(p))
    }
}

#[derive(Clone, Debug)]
pub struct UniquePath {
    system: SystemPath,
    id: Uuid,
}

impl UniquePath {
    pub fn new(protocol: Transport, address: IpAddr, port: u16, id: Uuid) -> UniquePath {
        UniquePath {
            system: SystemPath::new(protocol, address, port),
            id,
        }
    }

    pub fn with_system(system: SystemPath, id: Uuid) -> UniquePath {
        UniquePath { system, id }
    }

    pub fn with_socket(protocol: Transport, socket: SocketAddr, id: Uuid) -> UniquePath {
        UniquePath {
            system: SystemPath::with_socket(protocol, socket),
            id,
        }
    }

    pub fn uuid_ref(&self) -> &Uuid {
        &self.id
    }

    pub fn clone_id(&self) -> Uuid {
        self.id.clone()
    }
}

impl TryFrom<String> for UniquePath {
    type Error = PathParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let p = UniquePath::from_str(&s)?;
        Ok(p)
    }
}

/// Attempts to parse a `&str` as a [UniquePath].
impl FromStr for UniquePath {
    type Err = PathParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split("://").collect();
        // parts: [tcp]://[IP:port/path]
        if parts.len() != 2 {
            return Err(PathParseError::Form(s.to_string()));
        }
        let proto: Transport = parts[0].parse()?;
        let parts: Vec<&str> = parts[1].split('/').collect();
        // parts: [IP:port]/[UUID]
        if parts.len() != 2 {
            return Err(PathParseError::Form(s.to_string()));
        }
        let socket = SocketAddr::from_str(parts[0])?;
        let uuid =
            Uuid::from_str(parts[1]).map_err(|_parse_err| PathParseError::Form(s.to_string()))?;

        Ok(UniquePath::with_socket(proto, socket, uuid))
    }
}

impl SystemField for UniquePath {
    fn system(&self) -> &SystemPath {
        &self.system
    }
}

#[derive(Clone, Debug)]
pub struct NamedPath {
    system: SystemPath,
    path: Vec<String>,
}

impl NamedPath {
    pub fn new(protocol: Transport, address: IpAddr, port: u16, path: Vec<String>) -> NamedPath {
        NamedPath {
            system: SystemPath::new(protocol, address, port),
            path,
        }
    }

    pub fn with_socket(protocol: Transport, socket: SocketAddr, path: Vec<String>) -> NamedPath {
        NamedPath {
            system: SystemPath::with_socket(protocol, socket),
            path,
        }
    }

    pub fn with_system(system: SystemPath, path: Vec<String>) -> NamedPath {
        NamedPath { system, path }
    }

    pub fn path_ref(&self) -> &Vec<String> {
        &self.path
    }

    pub fn clone_path(&self) -> Vec<String> {
        self.path.clone()
    }
}

impl SystemField for NamedPath {
    fn system(&self) -> &SystemPath {
        &self.system
    }
}

impl TryFrom<String> for NamedPath {
    type Error = PathParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let p = NamedPath::from_str(&s)?;
        Ok(p)
    }
}

impl FromStr for NamedPath {
    type Err = PathParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s1: Vec<&str> = s.split("://").collect();
        if s1.len() != 2 {
            return Err(PathParseError::Form(s.to_string()));
        }
        let proto: Transport = s1[0].parse()?;
        let mut s2: Vec<&str> = s1[1].split('/').collect();
        if s2.len() < 1 {
            return Err(PathParseError::Form(s.to_string()));
        }
        let socket = SocketAddr::from_str(s2[0])?;
        let path: Vec<String> = if s2.len() > 1 {
            s2.split_off(1).into_iter().map(|s| s.to_string()).collect()
        } else {
            Vec::default()
        };
        Ok(NamedPath::with_socket(proto, socket, path.clone()))
    }
}

pub struct RegisteredPath {
    path: ActorPath,
    dispatcher: ActorRef,
}

impl Dispatching for RegisteredPath {
    fn dispatcher_ref(&self) -> ActorRef {
        self.dispatcher.clone()
    }
}

impl ActorSource for RegisteredPath {
    fn path_resolvable(&self) -> PathResolvable {
        PathResolvable::Path(self.path.clone())
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    const PATH: &'static str = "local://127.0.0.1:0/test_actor";

    #[test]
    fn actor_path_strings() {
        let mut settings = KompicsConfig::new();
        settings.label("my_system".to_string());
        //let system = KompicsSystem::new(settings);
        let ap = ActorPath::from_str(PATH);
        println!("Got ActorPath={}", ap.expect("a proper path"));
    }
}
