use crate::debugger::variable::VariableIR;
use std::borrow::Cow;

pub enum ValueRepr<'a> {
    PreRendered(Cow<'a, str>),
    Referential {
        addr: *const (),
        val: &'a VariableIR,
    },
    Wrapped(&'a VariableIR),
    Nested(&'a [VariableIR]),
}

pub trait RenderRepr {
    fn name(&self) -> &str;
    fn r#type(&self) -> &str;
    fn value(&self) -> Option<ValueRepr>;
}

impl RenderRepr for VariableIR {
    fn name(&self) -> &str {
        let name = match self {
            VariableIR::Scalar(s) => &s.name,
            VariableIR::Struct(s) => &s.name,
            VariableIR::Array(a) => &a.name,
            VariableIR::CEnum(e) => &e.name,
            VariableIR::RustEnum(e) => &e.name,
            VariableIR::Pointer(p) => &p.name,
        };

        let name = name.as_deref().unwrap_or("unknown");
        if name.starts_with("__") {
            let mb_num = name.trim_start_matches('_');
            if mb_num.parse::<u32>().is_ok() {
                return mb_num;
            }
        }
        name
    }

    fn r#type(&self) -> &str {
        let r#type = match self {
            VariableIR::Scalar(s) => &s.type_name,
            VariableIR::Struct(s) => &s.type_name,
            VariableIR::Array(a) => &a.type_name,
            VariableIR::CEnum(e) => &e.type_name,
            VariableIR::RustEnum(e) => &e.type_name,
            VariableIR::Pointer(p) => &p.type_name,
        };
        r#type.as_deref().unwrap_or("unknown")
    }

    fn value(&self) -> Option<ValueRepr> {
        let value_repr = match self {
            VariableIR::Scalar(scalar) => {
                ValueRepr::PreRendered(Cow::Owned(scalar.value.as_ref()?.to_string()))
            }
            VariableIR::Struct(r#struct) => ValueRepr::Nested(r#struct.members.as_ref()),
            VariableIR::Array(array) => ValueRepr::Nested(array.items.as_deref()?),
            VariableIR::CEnum(r#enum) => {
                ValueRepr::PreRendered(Cow::Borrowed(r#enum.value.as_ref()?))
            }
            VariableIR::RustEnum(r#enum) => ValueRepr::Wrapped(r#enum.value.as_ref()?),
            VariableIR::Pointer(pointer) => {
                let ptr = pointer.value?;
                let val = pointer.deref.as_ref()?;
                ValueRepr::Referential { addr: ptr, val }
            }
        };
        Some(value_repr)
    }
}