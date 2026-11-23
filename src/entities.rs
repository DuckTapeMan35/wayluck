use crate::Wayluck;
use std::{
    collections::HashMap,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Entity(u32);

pub struct Id<K>(Entity, PhantomData<fn() -> K>);

impl<K> Id<K> {
    pub fn entity(self) -> Entity {
        self.0
    }

    pub fn of_despawned(entity: Entity) -> Id<K> {
        Id(entity, PhantomData)
    }
}

impl<K> From<Id<K>> for u32 {
    fn from(id: Id<K>) -> u32 {
        (id.0).0
    }
}

impl<K> fmt::Display for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", (self.0).0)
    }
}

// Derived, these would require them of K.
impl<K> Clone for Id<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for Id<K> {}

impl<K> PartialEq for Id<K> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<K> Eq for Id<K> {}

impl<K> Hash for Id<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<K> fmt::Debug for Id<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", std::any::type_name::<K>(), (self.0).0)
    }
}

pub type Despawner = fn(&mut Wayluck, Entity);

#[derive(Default)]
pub struct Entities {
    next: u32,
    despawners: HashMap<Entity, Despawner>,
}

impl Entities {
    pub fn spawn<K>(&mut self, despawner: Despawner) -> Id<K> {
        self.next += 1;
        let entity = Entity(self.next);
        self.despawners.insert(entity, despawner);
        Id(entity, PhantomData)
    }

    pub fn exists(&self, entity: Entity) -> bool {
        self.despawners.contains_key(&entity)
    }

    fn take(&mut self, entity: Entity) -> Option<Despawner> {
        self.despawners.remove(&entity)
    }
}

impl Wayluck {
    pub fn spawn<K>(&mut self, despawner: Despawner) -> Id<K> {
        self.entities.spawn(despawner)
    }

    pub fn despawn(&mut self, entity: Entity) {
        if let Some(despawner) = self.entities.take(entity) {
            despawner(self, entity);
        }
    }

    pub fn run_systems(&mut self) {
        self.wm_windows_done();
        if let Err(e) = self.display_handle.flush_clients() {
            tracing::warn!("couldn't flush clients: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Thing {}
    enum Other {}

    fn nothing(_: &mut Wayluck, _: Entity) {}

    #[test]
    fn every_entity_is_numbered_once_whatever_its_kind() {
        let mut entities = Entities::default();
        let a: Id<Thing> = entities.spawn(nothing);
        let b: Id<Other> = entities.spawn(nothing);
        let c: Id<Thing> = entities.spawn(nothing);
        assert_ne!(a.entity(), b.entity());
        assert_ne!(a, c);
        entities.take(a.entity());
        let d: Id<Thing> = entities.spawn(nothing);
        assert!(![a, c].contains(&d));
        assert_ne!(d.entity(), b.entity());
    }

    #[test]
    fn an_entity_is_despawned_once() {
        let mut entities = Entities::default();
        let a: Id<Thing> = entities.spawn(nothing);
        assert!(entities.exists(a.entity()));
        assert!(entities.take(a.entity()).is_some());
        assert!(!entities.exists(a.entity()));
        assert!(entities.take(a.entity()).is_none());
    }
}
