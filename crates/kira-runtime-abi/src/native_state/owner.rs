use std::fmt;
use std::sync::Arc;

use super::NativeStateToken;

type OwnerEvent = Arc<dyn Fn(NativeStateToken) + Send + Sync>;

/// One callback-state ownership obligation carried inside the portable value tree.
///
/// Cloning this wrapper shares the *same* obligation and therefore does not
/// retain the state. That is what snapshots and aggregate copy-on-write need:
/// duplicating transport structure must never manufacture a Kira owner. Use
/// [`NativeStateOwner::duplicate_owner`] only when materializing another Kira
/// `NativeState<T>` value.
#[derive(Clone)]
pub struct NativeStateOwner {
    share: Arc<OwnerShare>,
}

struct OwnerShare {
    token: NativeStateToken,
    retain: OwnerEvent,
    release: OwnerEvent,
    armed: bool,
}

impl fmt::Debug for OwnerShare {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerShare")
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for NativeStateOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("NativeStateOwner")
            .field(&self.share.token)
            .finish()
    }
}

impl Drop for OwnerShare {
    fn drop(&mut self) {
        if self.armed {
            (self.release)(self.token);
        }
    }
}

impl NativeStateOwner {
    /// Takes over one already-owned state reference.
    pub fn new(
        token: NativeStateToken,
        retain: impl Fn(NativeStateToken) + Send + Sync + 'static,
        release: impl Fn(NativeStateToken) + Send + Sync + 'static,
    ) -> Self {
        Self {
            share: Arc::new(OwnerShare {
                token,
                retain: Arc::new(retain),
                release: Arc::new(release),
                armed: true,
            }),
        }
    }

    fn from_events(token: NativeStateToken, retain: OwnerEvent, release: OwnerEvent) -> Self {
        Self {
            share: Arc::new(OwnerShare {
                token,
                retain,
                release,
                armed: true,
            }),
        }
    }

    /// The opaque state token this obligation owns.
    pub fn token(&self) -> NativeStateToken {
        self.share.token
    }

    /// Moves this transport obligation into one Kira owner when no transport
    /// clone still shares it. A shared obligation cannot be stolen from the
    /// remaining snapshots, so the caller keeps the wrapper in that case.
    pub fn into_token(self) -> Result<NativeStateToken, Self> {
        match Arc::try_unwrap(self.share) {
            Ok(mut share) => {
                share.armed = false;
                Ok(share.token)
            }
            Err(share) => Err(Self { share }),
        }
    }

    /// Retains and returns a token for one newly materialized Kira owner.
    pub fn duplicate_token(&self) -> NativeStateToken {
        (self.share.retain)(self.share.token);
        self.share.token
    }

    /// Creates a second ownership obligation for a newly materialized transport owner.
    pub fn duplicate_owner(&self) -> Self {
        (self.share.retain)(self.share.token);
        Self::from_events(
            self.share.token,
            Arc::clone(&self.share.retain),
            Arc::clone(&self.share.release),
        )
    }
}

impl PartialEq for NativeStateOwner {
    fn eq(&self, other: &Self) -> bool {
        self.share.token == other.share.token
    }
}

impl Eq for NativeStateOwner {}
