use redis::{RedisWrite, ToRedisArgs};
use std::borrow::Cow;

pub struct LockResource<'a> {
    bytes: Cow<'a, [u8]>,
}

impl LockResource<'_> {
    pub fn to_vec(&self) -> Vec<u8> {
        self.bytes.to_vec()
    }
}

pub trait ToLockResource<'a> {
    fn to_lock_resource(self) -> LockResource<'a>;
}

impl<'a> ToLockResource<'a> for &'a LockResource<'a> {
    fn to_lock_resource(self) -> LockResource<'a> {
        LockResource {
            bytes: Cow::Borrowed(&*self.bytes),
        }
    }
}

impl<'a> ToLockResource<'a> for &'a [u8] {
    fn to_lock_resource(self) -> LockResource<'a> {
        LockResource {
            bytes: Cow::Borrowed(self),
        }
    }
}

impl<'a, const N: usize> ToLockResource<'a> for &'a [u8; N] {
    fn to_lock_resource(self) -> LockResource<'a> {
        LockResource {
            bytes: Cow::Borrowed(self),
        }
    }
}

impl<'a> ToLockResource<'a> for &'a Vec<u8> {
    fn to_lock_resource(self) -> LockResource<'a> {
        LockResource {
            bytes: Cow::Borrowed(self),
        }
    }
}

impl<'a> ToLockResource<'a> for &'a str {
    fn to_lock_resource(self) -> LockResource<'a> {
        LockResource {
            bytes: Cow::Borrowed(self.as_bytes()),
        }
    }
}

impl<'a> ToLockResource<'a> for &'a [&'a str] {
    fn to_lock_resource(self) -> LockResource<'a> {
        LockResource {
            bytes: Cow::Owned(self.join("").as_bytes().to_vec()),
        }
    }
}

impl<'a> ToLockResource<'a> for &'a [&'a [u8]] {
    fn to_lock_resource(self) -> LockResource<'a> {
        LockResource {
            bytes: Cow::Owned(self.concat()),
        }
    }
}

impl ToRedisArgs for LockResource<'_> {
    fn write_redis_args<W>(&self, out: &mut W)
    where
        W: ?Sized + RedisWrite,
    {
        self.bytes.write_redis_args(out);
    }
}
