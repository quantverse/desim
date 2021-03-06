/* Copyright © 2018 Gianmarco Garrisi

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program.  If not, see <http://www.gnu.org/licenses/>. */

//! This crate implement a discrete event simulation framework
//! inspired by the SimPy library for Python. It uses the generator
//! feature that is nightly. Once the feature is stabilized, also this
//! crate will use stable. Generators will be the only nightly feature
//! used in this crate.
//!
//! # Simulation
//! A simulation is performed scheduling one or more processes that
//! models the environment you are going to simulate. Your model may
//! consider some kind of finite resource that must be shared among
//! the processes, e.g. a bunch of servers in a simulation on queues.
//!
//! After setting up the simulation, it can be run step-by-step, using
//! the `step()` method, or all at once, with `run()`, until and ending
//! condition is met.
//!
//! The simulation will generate a log of all the events.
//!
/*
//! `nonblocking_run` lets you run the simulation in another thread
//! so that your program can go on without waiting for the simulation
//! to finish.
//!
*/
//! # Process
//! A process is implemented using the rust generators syntax.
//! This let us avoid the overhead of spawning a new thread for each
//! process, while still keeping the use of this framework quite simple.
//!
//! When a new process is created in the simulation, an identifier, of type
//! `ProcessId` is assigned to it. That id can be used to schedule an event that
//! resume the process.
//!
//! A process can be stopped and resumed later on. To stop the process, the
//! generator yields an `Effect` that specify what the simulator should do.
//! For example, a generator can set a timeout after witch it is executed again.
//! The process may also return. In that case it can not be resumed anymore.
//!
//!
//! # Resource
//! A resource is a finite amount of entities that can be used by one process
//! a time. When all the instances of the resource of interest are being used by
//! a process, the requiring one is enqueued in a FIFO and is resumed when the
//! resource become available again. When the process does not need the resource
//! anymore, it must release it.
//!
//! A resource can be created in the simulation using the `create_resource`
//! method, which requires the amount of resource and returns an identifier
//! for that resource that can be used to require and release it.
//!
//! A resource can be required and reelased by a process yielding
//! the corresponding `Effect`. There is no check on the fact that a process
//! yielding `Release` was holding a resource with that ID, but if a resource
//! gets more release then requests, the simulation will panic.
//!

#![feature(generators, generator_trait)]
use std::ops::{Generator, GeneratorState};
use std::collections::{BinaryHeap, VecDeque, HashMap, HashSet};
use std::cmp::{Ordering, Reverse};
use std::thread;
use std::pin::Pin;
use std::rc::Rc;
use std::cell::{Cell, RefCell};

/// The effect is yelded by a process generator to
/// interact with the simulation environment.
#[derive(Debug, Copy, Clone)]
pub enum Effect<T> {
    /// The process that yields this effect will be resumed
    /// after the speified time
    TimeOut(f64),
    /// Yielding this effect it is possible to schedule the specified event
    Event(Event),
    /// This effect is yielded to request a resource
    Request(ResourceId),
    /// This effect is yielded to release a resource that is not needed anymore.
    Release(ResourceId),
    /// Keep the process' state until it is resumed by another event.
    Wait,
    /// Interrupt another process
    Interrupt(ProcessId),
    /// Send message to process (with latency)
    SendMessage(ProcessId, T, f64)
}

/// Identifies a process. Can be used to resume it from another one and to schedule it.
pub type ProcessId = usize;
/// Identifies a resource. Can be used to request and release it.
pub type ResourceId = usize;

#[derive(Debug)]
struct Resource {
    allocated: usize,
    available: usize,
    queue: VecDeque<ProcessId>,
}

pub struct Context<T> {
    time: Cell<f64>,
    messages: RefCell<HashMap<ProcessId, VecDeque<T>>>,
    interrupted: RefCell<HashSet<ProcessId>>
}

impl<T> Context<T> {
    /// Create a new `Context` environment.
    pub fn new() -> Context<T> {
        Context::default()
    }

    /// Returns the current simulation time
    pub fn time(&self) -> f64 {
        self.time.get()
    }

    pub fn push_message(&self, pid: ProcessId, message: T) {
        let mut m = self.messages.borrow_mut();

        match m.get_mut(&pid) {
            Some(vd) => vd.push_back(message),
            None => {
                let mut vd = VecDeque::default();
                vd.push_back(message);
                m.insert(pid, vd);
            }
        }
    }

    pub fn pop_message(&self, pid: ProcessId) -> Option<T> {
        match self.messages.borrow_mut().get_mut(&pid) {
            Some(vd) => vd.pop_front(),
            None => None
        }
    }

    pub fn interrupt(&self, pid: ProcessId) {
        self.interrupted.borrow_mut().insert(pid);
    }

    pub fn check_interrupted(&self, pid: ProcessId) -> bool {
        self.interrupted.borrow_mut().remove(&pid)
    }
}


impl<T> Default for Context<T> {
    fn default() -> Self {
        Context {
            time: Cell::new(0.0),
            messages: RefCell::new(HashMap::default()),
            interrupted: RefCell::new(HashSet::default())
        }
    }
}

/// This struct provides the methods to create and run the simulation
/// in a single thread.
///
/// It provides methods to create processes and finite resources that
/// must be shared among them.
///
/// See the crate-level documentation for more information about how the
/// simulation framework works
pub struct Simulation<T> {
    context: Rc<Context<T>>,
    processes: HashMap<ProcessId, Option<Box<dyn Generator<Yield = Effect<T>, Return = ()> + Unpin>>>,
    future_events: BinaryHeap<Reverse<Event>>,
    processed_events: Vec<Event>,
    resources: Vec<Resource>,
}

/*
pub struct ParallelSimulation {
    processes: Vec<Box<Generator<Yield = Effect, Return = ()>>>
}
 */

/// An event that can be scheduled by a process, yelding the `Event` `Effect`
/// or by the owner of a `Simulation` through the `schedule` method
#[derive(Debug, Copy, Clone)]
pub struct Event {
    /// Time interval between the current simulation time and the event schedule
    pub time: f64,
    /// Process to execute when the event occur
    pub process: ProcessId,
}

/// Specify which condition must be met for the simulation to stop.
pub enum EndCondition {
    /// Run the simulation until a certain point in time is reached.
    Time(f64),
    /// Run the simulation until there are no more events scheduled.
    NoEvents,
    /// Execute exactly N steps of the simulation.
    NSteps(usize),
}

impl<T> Simulation<T> {
    /// Create a new `Simulation` environment.
    pub fn new(ctx: Rc<Context<T>>) -> Simulation<T> {
        Simulation {
            context: ctx,
            processes: HashMap::default(),
            future_events: BinaryHeap::default(),
            processed_events: Vec::default(),
            resources: Vec::default(),
        }
    }

    /// Returns the log of processed events
    pub fn processed_events(&self) -> &[Event] {
        self.processed_events.as_slice()
    }

    /// Create a process.
    ///
    /// For more information about a process, see the crate level documentation
    ///
    /// Returns the identifier of the process.
    pub fn create_process(
        &mut self,
        pid: ProcessId,
        process: Box<dyn Generator<Yield = Effect<T>, Return = ()> + Unpin>,
    ) {
        if self.processes.contains_key(&pid) {
            panic!("ERROR: duplicate PID {}", pid);
        }
        self.processes.insert(pid, Some(process));
    }

    /// Create a new finite resource, of which n instancies are available.
    ///
    /// For more information about a resource, see the crate level documentation
    ///
    /// Returns the identifier of the resource
    pub fn create_resource(&mut self, n: usize) -> ResourceId {
        let id = self.resources.len();
        self.resources.push(Resource {
            allocated: n,
            available: n,
            queue: VecDeque::new(),
        });
        id
    }

    /// Schedule a process to be executed. Another way to schedule events is
    /// yielding `Effect::Event` from a process during the simulation.
    pub fn schedule_event(&mut self, event: Event) {
        self.future_events.push(Reverse(event));
    }

    /// Proceed in the simulation by 1 step
    pub fn step(&mut self) {
        match self.future_events.pop() {
            Some(Reverse(event)) => {
                self.context.time.set(event.time);
                match Pin::new(self.processes.get_mut(&event.process).expect("No such process").as_mut().expect("ERROR. Tried to resume a completed process.")).resume() {
                    GeneratorState::Yielded(y) => match y {
                        Effect::TimeOut(t) => self.future_events.push(Reverse(Event {
                            time: self.context.time() + t,
                            process: event.process,
                        })),
                        Effect::Event(mut e) =>{
                            e.time += self.context.time();
                            self.future_events.push(Reverse(e))
                        },
                        Effect::Request(r) => {
                            let mut res = &mut self.resources[r];
                            if res.available == 0 {
                                // enqueue the process
                                res.queue.push_back(event.process);
                            } else {
                                // the process can use the resource immediately
                                self.future_events.push(Reverse(Event {
                                    time: self.context.time(),
                                    process: event.process,
                                }));
                                res.available -= 1;
                            }
                        }
                        Effect::Release(r) => {
                            let res = &mut self.resources[r];
                            match res.queue.pop_front() {
                                Some(p) =>
                                // some processes in queue: schedule the next.
                                    self.future_events.push(Reverse(Event{
                                        time: self.context.time(),
                                        process: p
                                    })),
                                None => {
                                    assert!(res.available < res.allocated);
                                    res.available += 1;
                                }
                            }
                            // after releasing the resource the process
                            // can be resumed
                            self.future_events.push(Reverse(Event {
                                time: self.context.time(),
                                process: event.process,
                            }))
                        }
                        Effect::Interrupt(pid) => {
                            self.context.interrupt(pid);
                            self.future_events.push(Reverse(Event {
                                time: self.context.time(),
                                process: pid,
                            }));
                            self.future_events.push(Reverse(Event {
                                time: self.context.time(),
                                process: event.process,
                            }))
                        }
                        Effect::SendMessage(pid, message, delay) => {
                            self.context.push_message(pid, message);
                            self.future_events.push(Reverse(Event {
                                time: self.context.time() + delay,
                                process: pid,
                            }));
                            self.future_events.push(Reverse(Event {
                                time: self.context.time(),
                                process: event.process,
                            }))
                        }
                        Effect::Wait => {}
                    },
                    GeneratorState::Complete(_) => {
                        // FIXME: removing the process from the vector would invalidate
                        // all existing `ProcessId`s, but keeping it would be a
                        // waste of space since it is completed.
                        // May be worth to use another data structure.
                        // At least let's remove the generator itself.
                        self.processes.get_mut(&event.process).expect("Invalid PID").take();
                    }
                }
                self.processed_events.push(event);
            }
            None => {}
        }
    }

    /// Run the simulation until and ending condition is met.
    pub fn run(mut self, until: EndCondition) -> Simulation<T> {
        while !self.check_ending_condition(&until) {
            self.step();
        }
        self
    }
/*
    pub fn nonblocking_run(mut self, until: EndCondition) -> thread::JoinHandle<Simulation> {
        thread::spawn(move || {
            self.run(until)
        })
    }
*/

    /// Return `true` if the ending condition was met, `false` otherwise.
    fn check_ending_condition(&self, ending_condition: &EndCondition) -> bool {
        match &ending_condition {
            EndCondition::Time(t) => if self.context.time() >= *t {
                return true
            },
            EndCondition::NoEvents => if self.future_events.len() == 0 {
                return true
            },
            // FIXME: what if client call `run(EndCondition::NSteps(n)` after having called `step()` for some times?
            EndCondition::NSteps(n) => if self.processed_events.len() == *n {
                return true
            },
        }
        false
    }
}

impl PartialEq for Event {
    fn eq(&self, other: &Event) -> bool {
        self.time == other.time
    }
}

impl Eq for Event {}

impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Event) -> Option<Ordering> {
        self.time.partial_cmp(&other.time)
    }
}

impl Ord for Event {
    fn cmp(&self, other: &Event) -> Ordering {
        match self.time.partial_cmp(&other.time) {
            Some(o) => o,
            None => panic!("Event time was uncomparable. Maybe a NaN"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use Context;

    #[derive(Debug, Copy, Clone, PartialEq)]
    enum TestMessage {
        MessageType1,
        MessageType2(&'static str)
    }

    #[test]
    fn it_works() {
        use Simulation;
        use Effect;
        use Event;

        let ctx = Rc::new(Context::<TestMessage>::new());
        let ctx2 = ctx.clone();
        let mut s = Simulation::new(ctx.clone());
        s.create_process(1, Box::new(move || {
            let mut a = 0.0;
            loop {
                a += 1.0;
                println!("time {}", ctx.time());
                
                yield Effect::TimeOut(a);
            }
        }));
        s.schedule_event(Event{time: 0.0, process: 1});
        s.step();
        s.step();
        assert_eq!(ctx2.time(), 1.0);
        s.step();
        assert_eq!(ctx2.time(), 3.0);
        s.step();
        assert_eq!(ctx2.time(), 6.0);
    }

    #[test]
    fn run() {
        use Simulation;
        use Effect;
        use Event;
        use EndCondition;

        let ctx = Rc::new(Context::<TestMessage>::new());
        let mut s = Simulation::new(ctx.clone());
        s.create_process(1,  Box::new(|| {
            let tik = 0.7;
            loop{
                println!("tik");
                yield Effect::TimeOut(tik);
            }
        }));
        s.schedule_event(Event{time: 0.0, process: 1});
        let s = s.run(EndCondition::Time(10.0));
        println!("{}", ctx.time());
        assert!(ctx.time() >= 10.0);
    }

    #[test]
    fn resource() {
        use Simulation;
        use Effect;
        use Event;
        use EndCondition::NoEvents;

        let ctx = Rc::new(Context::<TestMessage>::new());
        let mut s = Simulation::new(ctx.clone());
        let r = s.create_resource(1);

        // simple process that lock the resource for 7 time units
        s.create_process(1, Box::new(move || {
            yield Effect::Request(r);
            yield Effect::TimeOut(7.0);
            yield Effect::Release(r);
        }));
        // simple process that holds the resource for 3 time units
        s.create_process(2, Box::new(move || {
            yield Effect::Request(r);
            yield Effect::TimeOut(3.0);
            yield Effect::Release(r);
        }));

        // let p1 start immediately...
        s.schedule_event(Event{time: 0.0, process: 1});
        // let p2 start after 2 t.u., when r is not available
        s.schedule_event(Event{time: 2.0, process: 2});
        // p2 will wait r to be free (time 7.0) and its timeout
        // of 3.0 t.u. The simulation will end at time 10.0
        
        let s = s.run(NoEvents);
        println!("{:?}", s.processed_events());
        assert_eq!(ctx.time(), 10.0);
    }

    #[test]
    fn interruption() {
        use Simulation;
        use Effect;
        use Event;

        let ctx = Rc::new(Context::<TestMessage>::new());
        let ctx2 = ctx.clone();
        let mut s = Simulation::new(ctx.clone());
        s.create_process(1, Box::new(move || {
            yield Effect::TimeOut(1.0);
            println!("process #1: time {}", ctx.time());
            assert!(!ctx.check_interrupted(1));
            assert_eq!(ctx.time(), 1.0);

            yield Effect::TimeOut(1.0);
            println!("process #1: time {}", ctx.time());
            assert!(ctx.check_interrupted(1));
            assert_eq!(ctx.time(), 1.1);

        }));

        s.create_process(2, Box::new(move || {
            yield Effect::TimeOut(1.1);
            println!("{}: interrupting process #1", ctx2.time());
            yield Effect::Interrupt(1);
        }));

        s.schedule_event(Event{time: 0.0, process: 1});
        s.schedule_event(Event{time: 0.0, process: 2});
        s.step();
        s.step();
        s.step();
        s.step();
        s.step();
        s.step();
    }

    #[test]
    fn messaging() {
        use Simulation;
        use Effect;
        use Event;

        let ctx = Rc::new(Context::<TestMessage>::new());
        let ctx2 = ctx.clone();
        let mut s = Simulation::new(ctx.clone());
        s.create_process(1, Box::new(move || {
            yield Effect::Wait;
            println!("process #1: time {}", ctx.time());

            assert_eq!(ctx.time(), 1.2);

            let m1 = ctx.pop_message(1);
            assert_eq!(m1.expect("message expected"), TestMessage::MessageType2("hello there"));
            let m2 = ctx.pop_message(1);
            assert!(m2.is_none());
        }));

        s.create_process(2, Box::new(move || {
            yield Effect::TimeOut(1.0);
            println!("{}: sending message to process #1", ctx2.time());
            yield Effect::SendMessage(1, TestMessage::MessageType2("hello there"), 0.2);
        }));

        s.schedule_event(Event{time: 0.0, process: 1});
        s.schedule_event(Event{time: 0.0, process: 2});
        s.step();
        s.step();
        s.step();
        s.step();
        s.step();
    }
}
