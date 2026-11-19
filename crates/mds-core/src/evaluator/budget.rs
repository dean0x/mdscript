use std::{collections::hash_map, fmt, fmt::Write, slice};

use crate::{
    error::MdsError,
    limits::MAX_COMPILE_MEMORY,
    value::{Value, MAX_VALUE_DEPTH},
};

pub(crate) const NODE: usize = size_of::<Value>();
pub(super) const VALUE_ROOT: usize = NODE + 2 * size_of::<usize>();

pub(crate) fn bytes(count: usize, unit: usize) -> Option<usize> {
    count.checked_mul(unit)
}

pub(crate) fn display_len(value: &Value) -> Option<usize> {
    walk(value, Mode::Display)
}

pub(super) fn value_bytes(value: &Value) -> Option<usize> {
    walk(value, Mode::Bytes)
}

pub(crate) fn render_value(value: &Value, len: usize) -> Result<String, MdsError> {
    render_prefixed_value(value, len, "")
}

pub(crate) fn render_prefixed_value(
    value: &Value,
    len: usize,
    prefix: &str,
) -> Result<String, MdsError> {
    let mut text = String::with_capacity(
        len.checked_add(prefix.len())
            .ok_or_else(MdsError::compile_memory_exceeded)?,
    );
    text.push_str(prefix);
    write!(text, "{value}").map_err(|_| MdsError::compile_memory_exceeded())?;
    Ok(text)
}

#[derive(Clone, Copy)]
enum Mode {
    Bytes,
    Display,
}

enum Children<'a> {
    Array(slice::Iter<'a, Value>),
    Object(hash_map::Iter<'a, String, Value>),
}

pub(super) fn table_bytes(capacity: usize, entry: usize) -> Option<usize> {
    if capacity == 0 {
        return Some(0);
    }
    // Buckets include a control byte per entry and a trailing control group.
    let buckets = bytes(capacity, 8)?
        .div_ceil(7)
        .checked_next_power_of_two()?
        .max(4);
    bytes(buckets, entry.checked_add(1)?)?.checked_add(16)
}

fn walk(mut value: &Value, mode: Mode) -> Option<usize> {
    let mut total = match mode {
        Mode::Bytes => VALUE_ROOT,
        Mode::Display => 0,
    };
    let mut stack: [Option<Children<'_>>; MAX_VALUE_DEPTH] = std::array::from_fn(|_| None);
    let mut depth = 0;
    for _ in 0..MAX_COMPILE_MEMORY {
        let (n, children) = match (mode, value) {
            (Mode::Bytes, Value::String(s)) => (s.capacity(), None),
            (Mode::Display, Value::String(s)) => (s.len(), None),
            (Mode::Bytes, Value::Array(items)) => (
                bytes(items.capacity(), NODE)?,
                Some(Children::Array(items.iter())),
            ),
            (Mode::Display, Value::Array(items)) => (
                bytes(items.len().saturating_sub(1), 2)?,
                Some(Children::Array(items.iter())),
            ),
            (Mode::Bytes, Value::Object(fields)) => (
                table_bytes(fields.capacity(), size_of::<(String, Value)>())?,
                Some(Children::Object(fields.iter())),
            ),
            (Mode::Display, Value::Object(fields)) => (
                bytes(fields.len().saturating_sub(1), 2)?,
                Some(Children::Object(fields.iter())),
            ),
            (Mode::Display, Value::Number(_)) => (format_len(format_args!("{value}"))?, None),
            (Mode::Display, Value::Boolean(b)) => (if *b { 4 } else { 5 }, None),
            _ => (0, None),
        };
        total = total.checked_add(n)?;
        if let Some(children) = children {
            let nonempty = match &children {
                Children::Array(items) => items.len() != 0,
                Children::Object(fields) => fields.len() != 0,
            };
            if nonempty {
                *stack.get_mut(depth)? = Some(children);
                depth += 1;
            }
        }
        let mut next = None;
        for entry in stack[..depth].iter_mut().rev() {
            next = match entry.as_mut()? {
                Children::Array(items) => items.next(),
                Children::Object(fields) => match fields.next() {
                    Some((key, value)) => {
                        let key_bytes = match mode {
                            Mode::Bytes => key.capacity(),
                            Mode::Display => key.len().checked_add(2)?,
                        };
                        total = total.checked_add(key_bytes)?;
                        Some(value)
                    }
                    None => None,
                },
            };
            if next.is_some() {
                break;
            }
            depth -= 1;
        }
        match next {
            Some(child) => value = child,
            None => return Some(total),
        }
    }
    None
}

pub(crate) fn format_len(args: fmt::Arguments<'_>) -> Option<usize> {
    let mut len = Counter(0);
    len.write_fmt(args).ok()?;
    Some(len.0)
}

struct Counter(usize);

impl fmt::Write for Counter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0 = self.0.checked_add(s.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemBudget {
    pub(crate) headroom: usize,
    pub(crate) buffers: usize,
    temporaries: usize,
}

#[derive(Clone, Copy)]
pub(crate) enum Part {
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
    pub(crate) fn charge(&mut self, part: Part, n: usize) -> Result<(), MdsError> {
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

    pub(crate) fn mark(&self) -> usize {
        self.temporaries
    }

    pub(crate) fn release_to(&mut self, mark: usize) {
        if self.temporaries.checked_sub(mark).is_some() {
            self.temporaries = mark;
        }
    }

    pub(crate) fn release_buffers(&mut self, n: usize) {
        if let Some(buffers) = self.buffers.checked_sub(n) {
            self.buffers = buffers;
        }
    }

    pub(crate) fn transfer(&mut self, from: Part, to: Part, n: usize) {
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
