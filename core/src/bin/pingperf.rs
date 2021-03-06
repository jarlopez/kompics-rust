#![allow(unused_parens)]
extern crate kompact;
extern crate synchronoise;
extern crate time;

use kompact::*;
use std::sync::Arc;
use synchronoise::CountdownEvent;

#[inline(always)]
pub fn ignore<V>(_: V) -> () {
    ()
}

#[derive(Clone, Debug)]
struct Ping;
#[derive(Clone, Debug)]
struct Pong;

struct PingPongPort;

impl Port for PingPongPort {
    type Indication = Pong;
    type Request = Ping;
}

#[derive(ComponentDefinition, Actor)]
struct Pinger {
    ctx: ComponentContext<Pinger>,
    ppp: RequiredPort<PingPongPort, Pinger>,
    latch: Arc<CountdownEvent>,
    repeat: u64,
    sent: u64,
    received: u64,
}

impl Pinger {
    fn with(repeat: u64, latch: Arc<CountdownEvent>) -> Pinger {
        Pinger {
            ctx: ComponentContext::new(),
            ppp: RequiredPort::new(),
            latch,
            repeat,
            sent: 0,
            received: 0,
        }
    }
}

impl Provide<ControlPort> for Pinger {
    fn handle(&mut self, event: ControlEvent) -> () {
        match event {
            ControlEvent::Start => {
                println!("Starting Pinger...");
                let remaining = std::cmp::min(1000, self.repeat);
                for _ in 0..remaining {
                    self.ppp.trigger(Ping);
                    self.sent += 1;
                    //println!("Sent Ping #{}", self.sent);
                }
            }
            _ => (), // ignore
        }
    }
}

impl Require<PingPongPort> for Pinger {
    fn handle(&mut self, _event: Pong) -> () {
        self.received += 1;
        //println!("Received Pong #{}", self.received);
        if (self.sent < self.repeat) {
            self.ppp.trigger(Ping);
            self.sent += 1;
        //println!("Sent Ping #{}", self.sent);
        } else if (self.received >= self.repeat) {
            ignore(self.latch.decrement());
        }
    }
}

#[derive(ComponentDefinition, Actor)]
struct Ponger {
    ctx: ComponentContext<Ponger>,
    ppp: ProvidedPort<PingPongPort, Ponger>,
    received: u64,
}

impl Ponger {
    fn new() -> Ponger {
        Ponger {
            ctx: ComponentContext::new(),
            ppp: ProvidedPort::new(),
            received: 0,
        }
    }
}

impl Provide<ControlPort> for Ponger {
    fn handle(&mut self, event: ControlEvent) -> () {
        match event {
            ControlEvent::Start => {
                println!("Ponger starting...");
            }
            _ => (), // ignore
        }
    }
}

impl Provide<PingPongPort> for Ponger {
    fn handle(&mut self, _event: Ping) -> () {
        self.received += 1;
        //println!("Got Ping #{}", self.received);
        self.ppp.trigger(Pong);
    }
}

const NS_TO_S: f64 = 1.0 / (1000.0 * 1000.0 * 1000.0);
const MSGS: u64 = 5000000;
const PROC_PAIRS: usize = 8;

fn main() {
    println!("Starting system");
    let sys = KompicsSystem::default();
    let latch = Arc::new(CountdownEvent::new(PROC_PAIRS));
    let ref l2 = latch;
    let mut pingers = Vec::<Arc<Component<Pinger>>>::new();
    let mut pongers = Vec::<Arc<Component<Ponger>>>::new();
    for _ in 0..PROC_PAIRS {
        let pingerc = sys.create(move || Pinger::with(MSGS, l2.clone()));
        let pongerc = sys.create(move || Ponger::new());
        on_dual_definition(&pingerc, &pongerc, |pingercd, pongercd| {
            biconnect(&mut pongercd.ppp, &mut pingercd.ppp);
        });
        pingers.push(pingerc);
        pongers.push(pongerc);
    }
    println!("Starting {} Pingers&Pongers", PROC_PAIRS);
    let startt = time::precise_time_ns();
    for i in 0..PROC_PAIRS {
        sys.start(&pongers[i as usize]);
        sys.start(&pingers[i as usize]);
    }
    println!("Waiting for countdown...");
    latch.wait();
    let endt = time::precise_time_ns();
    println!("all done!");
    let difft = (endt - startt) as f64;
    let diffts = difft * NS_TO_S;
    let total_msgs = (MSGS * (PROC_PAIRS as u64)) as f64;
    let msgs = total_msgs / diffts;
    println!("Ran {}msgs/s", msgs);
    sys.shutdown().expect("Kompics didn't shut down properly");
}
