use crate::{error::MdsError, limits::MAX_COMPILE_MEMORY};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MemBudget {
    headroom: usize,
    buffers: usize,
    temporaries: usize,
}

#[derive(Clone, Copy)]
pub(super) enum Part {
    Buffers,
    Temp,
}

impl Default for MemBudget {
    fn default() -> Self {
        Self {
            headroom: MAX_COMPILE_MEMORY,
            buffers: 0,
            temporaries: 0,
        }
    }
}

impl MemBudget {
    #[inline(never)]
    pub(super) fn charge(&mut self, part: Part, n: usize) -> Result<(), MdsError> {
        let total = self
            .buffers
            .checked_add(self.temporaries)
            .and_then(|used| used.checked_add(n));
        if total.is_none_or(|total| total > self.headroom) {
            return Err(MdsError::compile_memory_exceeded());
        }
        match part {
            Part::Buffers => self.buffers += n,
            Part::Temp => self.temporaries += n,
        }
        Ok(())
    }

    pub(super) fn mark(&self) -> usize {
        self.temporaries
    }

    pub(super) fn release_to(&mut self, mark: usize) {
        if self.temporaries.checked_sub(mark).is_some() {
            self.temporaries = mark;
        }
    }

    pub(super) fn release_buffers(&mut self, n: usize) {
        if let Some(buffers) = self.buffers.checked_sub(n) {
            self.buffers = buffers;
        }
    }

    pub(super) fn transfer(&mut self, from: Part, to: Part, n: usize) {
        let (from, to) = match (from, to) {
            (Part::Buffers, Part::Temp) => (&mut self.buffers, &mut self.temporaries),
            (Part::Temp, Part::Buffers) => (&mut self.temporaries, &mut self.buffers),
            _ => return,
        };
        if let Some(left) = from.checked_sub(n) {
            *from = left;
            // The sum was checked at charge time. A transfer does not change it.
            *to += n;
        }
    }
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
