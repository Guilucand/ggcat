use crate::execution_manager::objects_pool::{ObjectsPool, PoolObjectTrait};
use crossbeam::channel::Sender;
use std::any::Any;
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub trait PacketTrait: PoolObjectTrait + Sync + Send {}

trait PacketPoolReturnerTrait: Send + Sync {
    fn send_any(&self, packet: Box<dyn Any>);
}

impl<T: Send + Sync + 'static> PacketPoolReturnerTrait for Sender<Box<T>> {
    fn send_any(&self, packet: Box<dyn Any>) {
        self.send(packet.downcast().unwrap());
    }
}

pub struct Packet<T: 'static> {
    object: MaybeUninit<Box<T>>,
    returner: Option<Arc<dyn PacketPoolReturnerTrait>>,
}

pub struct PacketAny {
    object: MaybeUninit<Box<dyn Any + Send + Sync>>,
    returner: Option<Arc<dyn PacketPoolReturnerTrait>>,
}

pub struct PacketsPool<T>(ObjectsPool<Box<T>>, Arc<dyn PacketPoolReturnerTrait>);

// Recursively implement the object trait for the pool, so it can be used recursively
impl<T: PacketTrait> PoolObjectTrait for PacketsPool<T> {
    type InitData = (usize, bool, T::InitData);

    fn allocate_new((cap, strict_cap, init_data): &Self::InitData) -> Self {
        Self::new(*cap, *strict_cap, init_data.clone())
    }

    fn reset(&mut self) {}
}

impl<T: PacketTrait> PacketsPool<T> {
    pub fn new(cap: usize, strict_capacity: bool, init_data: T::InitData) -> Self {
        let pool = ObjectsPool::new(cap, strict_capacity, init_data);
        let returner = Arc::new(pool.channel.0.clone());
        Self(pool, returner)
    }

    pub fn alloc_packet(&self) -> Packet<T> {
        let mut object = self.0.alloc_object();

        let packet = Packet {
            object: MaybeUninit::new(unsafe { object.value.assume_init_read() }),
            returner: Some(self.1.clone()),
        };

        unsafe {
            std::ptr::drop_in_place(&mut object.ref_pool);
            std::mem::forget(object);
        }

        packet
    }
}

impl<T: Any + Send + Sync> Packet<T> {
    pub fn new_simple(data: T) -> Self {
        Packet {
            object: MaybeUninit::new(Box::new(data)),
            returner: None,
        }
    }

    pub fn upcast(mut self) -> PacketAny {
        let packet = PacketAny {
            object: MaybeUninit::new(unsafe { self.object.assume_init_read() }),
            returner: self.returner.clone(),
        };

        unsafe {
            std::ptr::drop_in_place(&mut self.returner);
            std::mem::forget(self);
        }

        packet
    }
}

impl<T: Any + Send + Sync> Deref for Packet<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.object.assume_init_ref() }
    }
}

impl<T: Any + Send + Sync> DerefMut for Packet<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.object.assume_init_mut() }
    }
}

impl PacketAny {
    pub fn downcast<T: 'static>(mut self) -> Packet<T> {
        let packet = Packet {
            object: MaybeUninit::new(unsafe { self.object.assume_init_read().downcast().unwrap() }),
            returner: self.returner.clone(),
        };

        unsafe {
            std::ptr::drop_in_place(&mut self.returner);
            std::mem::forget(self);
        }

        packet
    }
}

impl<T: 'static> Drop for Packet<T> {
    fn drop(&mut self) {
        if let Some(returner) = &self.returner {
            let value = unsafe { self.object.assume_init_read() };
            returner.send_any(value);
        } else {
            unsafe { self.object.assume_init_drop() }
        }
    }
}

impl Drop for PacketAny {
    fn drop(&mut self) {
        panic!("Cannot drop packet any!");
    }
}

impl PoolObjectTrait for () {
    type InitData = ();
    fn allocate_new(init_data: &Self::InitData) -> Self {
        panic!("Cannot create () type as object!");
    }

    fn reset(&mut self) {
        panic!("Cannot reset () type as object!");
    }
}
impl PacketTrait for () {}