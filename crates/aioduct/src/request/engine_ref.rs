use std::ops::Deref;

pub(crate) enum EngineRef<'a, E: Clone> {
    Borrowed(&'a E),
    Owned(Box<E>),
}

impl<'a, E: Clone> EngineRef<'a, E> {
    pub(crate) fn try_clone_for_lifetime(&self) -> EngineRef<'a, E> {
        match self {
            EngineRef::Borrowed(r) => EngineRef::Borrowed(r),
            EngineRef::Owned(o) => EngineRef::Owned(o.clone()),
        }
    }
}

impl<E: Clone> Deref for EngineRef<'_, E> {
    type Target = E;

    fn deref(&self) -> &E {
        match self {
            EngineRef::Borrowed(r) => r,
            EngineRef::Owned(o) => o,
        }
    }
}
