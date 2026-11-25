use crate::error::MdsError;
use crate::evaluator::budget::{value_bytes, MemBudget, Part};
use crate::limits::MAX_DATA_FILE_BYTES;
use crate::resolver::{parse_data_json_with, parse_data_yaml_with, Allowance, NODE_YAML};
use crate::value::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Accept {
    Json,
    Yaml,
    Text,
}

#[derive(Clone, Copy)]
pub(crate) struct DataLabel<'a> {
    /// Project-relative label of the declaring module.
    pub(crate) module: &'a str,
    pub(crate) entry: &'a str,
    pub(crate) file: Option<&'a str>,
}

impl DataLabel<'_> {
    pub(crate) fn syntax(self, format: &str) -> MdsError {
        MdsError::yaml_error(format!(
            "data file \"{}\", entry \"{}\": invalid {format} data",
            crate::escape_path_for_message(self.file.unwrap_or(self.module)),
            crate::sanitize_control_chars_wire(self.entry),
        ))
    }

    pub(crate) fn error(self, field: Option<&str>, reason: &str) -> MdsError {
        MdsError::data(self.module, Some(self.entry), field, reason, None)
    }
}

/// Decode external data and retain its value charge only on success.
#[must_use = "data conversion can fail"]
pub(crate) fn convert(
    bytes: &[u8],
    accept: Accept,
    label: DataLabel<'_>,
    mem: &mut MemBudget,
) -> Result<Value, MdsError> {
    let max = MAX_DATA_FILE_BYTES as usize;
    if bytes.len() > max {
        return Err(MdsError::resource_limit(format!(
            "data file too large ({} bytes, max {max} bytes)",
            bytes.len()
        )));
    }
    let raw = std::str::from_utf8(bytes).map_err(|_| label.error(None, "invalid UTF-8"))?;
    let bom = char::from_u32(0xfeff).filter(|&ch| raw.starts_with(ch));
    let raw = if let Some(bom) = bom {
        let rest = &raw[bom.len_utf8()..];
        if rest.starts_with(bom) {
            return Err(label.error(None, "expected at most one leading byte order mark"));
        }
        rest
    } else {
        raw
    };

    let allowance = Allowance::in_compile(mem);
    let mark = mem.mark();
    let converted = match accept {
        Accept::Yaml | Accept::Json => {
            let (tree, spent) = match accept {
                Accept::Json => parse_data_json_with(raw, allowance, label),
                _ => parse_data_yaml_with(raw, allowance, label),
            }?;
            // Reserve space for both trees while they are live. The node charge
            // covers either representation.
            mem.charge(Part::Temp, spent.checked_mul(2))?;
            Value::from_data_yaml(&tree, label)
        }
        Accept::Text => {
            mem.charge(Part::Temp, NODE_YAML.checked_add(raw.len()))?;
            Value::from_data_text(raw, label)
        }
    };
    mem.release_to(mark);
    let value = converted?;
    mem.charge(Part::Buffers, value_bytes(&value))?;
    Ok(value)
}
