use alloc::arc::Arc;
use alloc::BTreeMap;
use core::sync::atomic::{AtomicUsize, Ordering, ATOMIC_USIZE_INIT};
use spin::{Once, RwLock, RwLockReadGuard, RwLockWriteGuard};

use context;
use scheme::{self, SchemeId};
use sync::WaitQueue;
use syscall::data::Event;
use syscall::error::{Error, Result, EBADF, ESRCH};

int_like!(EventQueueId, AtomicEventQueueId, usize, AtomicUsize);

pub struct EventQueue {
    id: EventQueueId,
    queue: WaitQueue<Event>,
}

impl EventQueue {
    pub fn new(id: EventQueueId) -> EventQueue {
        EventQueue {
            id: id,
            queue: WaitQueue::new()
        }
    }

    pub fn dup(&self, other: &EventQueue) {
        panic!("EventQeuue::dup");
    }

    pub fn read(&self, events: &mut [Event]) -> Result<usize> {
        Ok(self.queue.receive_into(events, true))
    }

    pub fn write(&self, events: &[Event]) -> Result<usize> {
        for event in events {
            let file = {
                let contexts = context::contexts();
                let context_lock = contexts.current().ok_or(Error::new(ESRCH))?;
                let context = context_lock.read();
                let mut files = context.files.lock();
                match files.get(event.id).ok_or(Error::new(EBADF))? {
                    Some(file) => file.clone(),
                    None => return Err(Error::new(EBADF))
                }
            };

            let (scheme, number) = {
                let description = file.description.read();
                (description.scheme, description.number)
            };

            register(
                RegKey { scheme, number },
                QueueKey { queue: self.id, id: event.id, data: event.data },
                event.flags
            );

            send_flags(RegKey { scheme, number })?;
        }

        Ok(events.len())
    }
}

pub type EventQueueList = BTreeMap<EventQueueId, Arc<EventQueue>>;

// Next queue id
static NEXT_QUEUE_ID: AtomicUsize = ATOMIC_USIZE_INIT;

/// Get next queue id
pub fn next_queue_id() -> EventQueueId {
    EventQueueId::from(NEXT_QUEUE_ID.fetch_add(1, Ordering::SeqCst))
}

// Current event queues
static QUEUES: Once<RwLock<EventQueueList>> = Once::new();

/// Initialize queues, called if needed
fn init_queues() -> RwLock<EventQueueList> {
    RwLock::new(BTreeMap::new())
}

/// Get the event queues list, const
pub fn queues() -> RwLockReadGuard<'static, EventQueueList> {
    QUEUES.call_once(init_queues).read()
}

/// Get the event queues list, mutable
pub fn queues_mut() -> RwLockWriteGuard<'static, EventQueueList> {
    QUEUES.call_once(init_queues).write()
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegKey {
    pub scheme: SchemeId,
    pub number: usize,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct QueueKey {
    pub queue: EventQueueId,
    pub id: usize,
    pub data: usize
}

type Registry = BTreeMap<RegKey, BTreeMap<QueueKey, usize>>;

static REGISTRY: Once<RwLock<Registry>> = Once::new();

/// Initialize registry, called if needed
fn init_registry() -> RwLock<Registry> {
    RwLock::new(Registry::new())
}

/// Get the global schemes list, const
fn registry() -> RwLockReadGuard<'static, Registry> {
    REGISTRY.call_once(init_registry).read()
}

/// Get the global schemes list, mutable
pub fn registry_mut() -> RwLockWriteGuard<'static, Registry> {
    REGISTRY.call_once(init_registry).write()
}

pub fn register(reg_key: RegKey, queue_key: QueueKey, flags: usize) {
    let mut registry = registry_mut();

    let entry = registry.entry(reg_key).or_insert_with(|| {
        BTreeMap::new()
    });

    if flags == 0 {
        entry.remove(&queue_key);
    } else {
        entry.insert(queue_key, flags);
    }
}

pub fn send_flags(reg_key: RegKey) -> Result<()> {
    let mut flags = 0;

    {
        let registry = registry();

        if let Some(queue_list) = registry.get(&reg_key) {
            for (_queue_key, queue_flags) in queue_list.iter() {
                flags |= queue_flags;
            }
        }
    }

    let event_id = {
        let scheme = {
            let schemes = scheme::schemes();
            let scheme = schemes.get(reg_key.scheme).ok_or(Error::new(EBADF))?;
            Arc::clone(&scheme)
        };

        scheme.fevent(reg_key.number, flags)?
    };

    if event_id != reg_key.number {
        println!("scheme {} returned event id {} instead of {}", reg_key.scheme.into(), event_id, reg_key.number);
    }

    Ok(())
}

pub fn unregister_file(scheme: SchemeId, number: usize) {
    let mut registry = registry_mut();

    registry.remove(&RegKey { scheme, number });
}

//TODO: Implement unregister_queue
// pub fn unregister_queue(scheme: SchemeId, number: usize) {
//
// }

pub fn trigger(scheme: SchemeId, number: usize, flags: usize) {
    let registry = registry();

    if let Some(queue_list) = registry.get(&RegKey { scheme, number }) {
        for (queue_key, queue_flags) in queue_list.iter() {
            let common_flags = flags & queue_flags;
            if common_flags != 0 {
                let queues = queues();
                if let Some(queue) = queues.get(&queue_key.queue) {
                    queue.queue.send(Event {
                        id: queue_key.id,
                        flags: common_flags,
                        data: queue_key.data
                    });
                }
            }
        }
    }
}