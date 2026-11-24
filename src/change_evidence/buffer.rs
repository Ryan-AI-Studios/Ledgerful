//! Account for owned byte buffers within one repository observation.
use std::cell::Cell;

pub(crate) struct MemoryBudget {
    limit: usize,
    available: Cell<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MemoryLimit;

impl MemoryBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            available: Cell::new(limit),
        }
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    pub(crate) fn remaining(&self) -> usize {
        self.available.get()
    }

    /// Transfer an existing buffer into the observation's accounting.
    pub(crate) fn hold(&self, bytes: Vec<u8>) -> Result<ReservedBytes<'_>, MemoryLimit> {
        let reservation = self.reserve(bytes.capacity())?;
        Ok(ReservedBytes {
            bytes,
            _reservation: reservation,
        })
    }

    pub(crate) fn reserve(&self, capacity: usize) -> Result<Reservation<'_>, MemoryLimit> {
        let remaining = self.remaining().checked_sub(capacity).ok_or(MemoryLimit)?;
        self.available.set(remaining);
        Ok(Reservation {
            capacity,
            budget: self,
        })
    }
}

pub(crate) struct Reservation<'a> {
    capacity: usize,
    budget: &'a MemoryBudget,
}

impl<'a> Reservation<'a> {
    pub(crate) fn account_capacity(&mut self, capacity: usize) -> Result<(), MemoryLimit> {
        let remaining = (self.budget.remaining() + self.capacity)
            .checked_sub(capacity)
            .ok_or(MemoryLimit)?;
        self.budget.available.set(remaining);
        self.capacity = capacity;
        Ok(())
    }

    pub(crate) fn into_bytes(mut self, bytes: Vec<u8>) -> Result<ReservedBytes<'a>, MemoryLimit> {
        self.account_capacity(bytes.capacity())?;
        Ok(ReservedBytes {
            bytes,
            _reservation: self,
        })
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.budget
            .available
            .set(self.budget.remaining() + self.capacity);
    }
}

/// The backing allocation remains charged for as long as this value is held.
pub(crate) struct ReservedBytes<'a> {
    bytes: Vec<u8>,
    _reservation: Reservation<'a>,
}

impl AsRef<[u8]> for ReservedBytes<'_> {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquisition_charges_capacity_until_the_caller_drops_the_bytes() {
        let mut bytes = Vec::with_capacity(8);
        bytes.extend_from_slice(b"abc");
        let capacity = bytes.capacity();
        let budget = MemoryBudget::new(capacity + 5);

        let held = budget.hold(bytes).unwrap();
        assert_eq!(held.as_ref(), b"abc");
        assert_eq!(budget.limit(), capacity + 5);
        assert_eq!(budget.remaining(), 5);

        drop(held);
        assert_eq!(budget.remaining(), capacity + 5);
    }

    #[test]
    fn dropping_one_buffer_leaves_the_other_buffer_charged() {
        let first = vec![1, 2, 3];
        let second = vec![4, 5];
        let (first_size, second_size) = (first.capacity(), second.capacity());
        let budget = MemoryBudget::new(first_size + second_size);
        let first = budget.hold(first).unwrap();
        let second = budget.hold(second).unwrap();
        assert_eq!(budget.remaining(), 0);

        drop(first);
        assert_eq!(budget.remaining(), first_size);
        assert_eq!(second.as_ref(), &[4, 5]);

        drop(second);
        assert_eq!(budget.remaining(), first_size + second_size);
    }

    #[test]
    fn refusing_an_over_limit_buffer_leaves_observation_accounts_independent() {
        let bytes = vec![1, 2, 3];
        let capacity = bytes.capacity();
        let first = MemoryBudget::new(capacity);
        let second = MemoryBudget::new(capacity);
        let held = first.hold(bytes).unwrap();

        assert!(matches!(first.hold(vec![4]), Err(MemoryLimit)));
        assert_eq!(first.remaining(), 0);
        assert_eq!(second.remaining(), capacity);

        let other = second.hold(vec![4]).unwrap();
        assert_eq!(other.as_ref(), &[4]);
        drop(held);
        assert_eq!(first.remaining(), capacity);
    }

    #[test]
    fn a_preallocation_reservation_refunds_on_drop_or_transfers_to_owned_bytes() {
        let budget = MemoryBudget::new(8);
        let first = budget.reserve(5).unwrap();
        assert_eq!(budget.remaining(), 3);
        assert!(matches!(budget.reserve(4), Err(MemoryLimit)));
        drop(first);
        assert_eq!(budget.remaining(), 8);

        let mut reservation = budget.reserve(4).unwrap();
        assert_eq!(reservation.account_capacity(9), Err(MemoryLimit));
        assert_eq!(budget.remaining(), 4);
        let bytes = vec![1, 2, 3, 4];
        let capacity = bytes.capacity();
        let held = reservation.into_bytes(bytes).unwrap();
        assert_eq!(budget.remaining(), 8 - capacity);
        assert_eq!(held.as_ref(), &[1, 2, 3, 4]);
        drop(held);
        assert_eq!(budget.remaining(), 8);
    }
}
