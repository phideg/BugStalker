pub mod eval;
pub mod parser;
mod symbol;
pub mod r#type;

use crate::debugger::dwarf::eval::EvalOption;
use crate::debugger::dwarf::parser::unit::{
    DieVariant, Entry, FunctionDie, Node, Unit, VariableDie,
};
use crate::debugger::dwarf::parser::DieRef;
use crate::debugger::dwarf::r#type::EvaluationContext;
use crate::debugger::dwarf::symbol::SymbolTab;
use crate::debugger::TypeDeclaration;
use crate::weak_error;
use anyhow::anyhow;
use bytes::Bytes;
use fallible_iterator::FallibleIterator;
use gimli::{DebugInfoOffset, Dwarf, RunTimeEndian, UnitOffset};
use nix::unistd::Pid;
use object::{Object, ObjectSection};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::rc::Rc;
pub use symbol::Symbol;

pub type EndianRcSlice = gimli::EndianRcSlice<gimli::RunTimeEndian>;

#[derive(Default)]
pub struct DebugeeContextBuilder();

impl DebugeeContextBuilder {
    fn load_section<'a: 'b, 'b, OBJ, Endian>(
        id: gimli::SectionId,
        file: &'a OBJ,
        endian: Endian,
    ) -> anyhow::Result<gimli::EndianRcSlice<Endian>>
    where
        OBJ: object::Object<'a, 'b>,
        Endian: gimli::Endianity,
    {
        let data = file
            .section_by_name(id.name())
            .and_then(|section| section.uncompressed_data().ok())
            .unwrap_or(Cow::Borrowed(&[]));
        Ok(gimli::EndianRcSlice::new(Rc::from(&*data), endian))
    }

    pub fn build<'a, 'b, OBJ>(
        &self,
        obj_file: &'a OBJ,
    ) -> anyhow::Result<DebugeeContext<EndianRcSlice>>
    where
        'a: 'b,
        OBJ: Object<'a, 'b>,
    {
        let endian = if obj_file.is_little_endian() {
            RunTimeEndian::Little
        } else {
            RunTimeEndian::Big
        };

        let dwarf = gimli::Dwarf::load(|id| Self::load_section(id, obj_file, endian))?;
        let symbol_table = SymbolTab::new(obj_file);

        let parser = parser::DwarfUnitParser::new(&dwarf);

        let mut units = dwarf
            .units()
            .map(|header| parser.parse(dwarf.unit(header)?))
            .collect::<Vec<_>>()?;
        units.sort_by_key(|u| u.offset);

        Ok(DebugeeContext {
            _inner: dwarf,
            units,
            symbol_table,
        })
    }
}

pub struct DebugeeContext<R: gimli::Reader = EndianRcSlice> {
    _inner: Dwarf<R>,
    units: Vec<parser::unit::Unit>,
    symbol_table: Option<SymbolTab>,
}

impl DebugeeContext {
    fn find_unit_by_pc(&self, pc: u64) -> Option<&parser::unit::Unit> {
        self.units.iter().find(
            |&unit| match unit.ranges.binary_search_by_key(&pc, |r| r.begin) {
                Ok(_) => true,
                Err(pos) => unit.ranges[..pos]
                    .iter()
                    .rev()
                    .any(|range| range.begin <= pc && pc <= range.end),
            },
        )
    }

    pub fn find_place_from_pc(&self, pc: usize) -> Option<parser::unit::Place> {
        let pc = pc as u64;
        let unit = self.find_unit_by_pc(pc)?;
        unit.find_place_by_pc(pc)
    }

    pub fn find_function_by_pc(&self, pc: usize) -> Option<ContextualDieRef<FunctionDie>> {
        let pc = pc as u64;
        let unit = self.find_unit_by_pc(pc)?;

        let find_pos = match unit
            .die_ranges
            .binary_search_by_key(&pc, |dr| dr.range.begin)
        {
            Ok(pos) => pos + 1,
            Err(pos) => pos,
        };

        unit.die_ranges[..find_pos].iter().rev().find_map(|dr| {
            if let DieVariant::Function(ref func) = unit.entries[dr.die_idx].die {
                if dr.range.begin <= pc && pc <= dr.range.end {
                    return Some(ContextualDieRef {
                        context: self,
                        node: &unit.entries[dr.die_idx].node,
                        unit,
                        die: func,
                    });
                }
            };
            None
        })
    }

    pub fn find_function_by_name(&self, needles: &str) -> Option<ContextualDieRef<FunctionDie>> {
        self.units.iter().find_map(|unit| {
            unit.entries.iter().find_map(|entry| {
                if let DieVariant::Function(func) = &entry.die {
                    if func.base_attributes.name.as_deref() == Some(needles) {
                        return Some(ContextualDieRef {
                            context: self,
                            unit,
                            node: &entry.node,
                            die: func,
                        });
                    }
                }
                None
            })
        })
    }

    pub fn find_stmt_line(&self, file: &str, line: u64) -> Option<parser::unit::Place<'_>> {
        self.units
            .iter()
            .find_map(|unit| unit.find_stmt_line(file, line))
    }

    pub fn find_symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbol_table.as_ref().and_then(|table| table.get(name))
    }

    pub fn deref_die<'this>(
        &'this self,
        default_unit: &'this Unit,
        reference: DieRef,
    ) -> Option<&'this Entry> {
        match reference {
            DieRef::Unit(offset) => default_unit.find_entry(offset),
            DieRef::Global(offset) => {
                let unit = match self.units.binary_search_by_key(&Some(offset), |u| u.offset) {
                    Ok(_) | Err(0) => return None,
                    Err(pos) => &self.units[pos - 1],
                };
                unit.find_entry(UnitOffset(
                    offset.0 - unit.offset.unwrap_or(DebugInfoOffset(0)).0,
                ))
            }
        }
    }
}

pub struct ContextualDieRef<'a, T> {
    pub context: &'a DebugeeContext,
    pub unit: &'a Unit,
    pub node: &'a Node,
    pub die: &'a T,
}

impl<'a, T> Clone for ContextualDieRef<'a, T> {
    fn clone(&self) -> Self {
        Self {
            context: self.context,
            unit: self.unit,
            node: self.node,
            die: self.die,
        }
    }
}

impl<'a, T> Copy for ContextualDieRef<'a, T> {}

impl<'ctx> ContextualDieRef<'ctx, FunctionDie> {
    pub fn frame_base_addr(&self, pid: Pid) -> anyhow::Result<usize> {
        let attr = self
            .die
            .fb_addr
            .as_ref()
            .ok_or_else(|| anyhow!("no frame base attr"))?;
        let expr = attr
            .exprloc_value()
            .ok_or_else(|| anyhow!("frame base attribute not an expression"))?;

        let result = self
            .unit
            .evaluator(pid)
            .evaluate(expr)?
            .into_scalar::<usize>()?;
        Ok(result)
    }

    pub fn find_variables<'this>(
        &'this self,
        pc: usize,
    ) -> Vec<ContextualDieRef<'ctx, VariableDie>> {
        let mut result = vec![];
        let mut queue = VecDeque::from(self.node.children.clone());
        while let Some(idx) = queue.pop_front() {
            if let DieVariant::Variable(ref var) = self.unit.entries[idx].die {
                let var_ref = ContextualDieRef {
                    context: self.context,
                    unit: self.unit,
                    node: &self.unit.entries[idx].node,
                    die: var,
                };

                if var_ref.valid_at(pc) {
                    result.push(var_ref);
                }
            }
            self.unit.entries[idx]
                .node
                .children
                .iter()
                .for_each(|i| queue.push_back(*i));
        }
        result
    }
}

impl<'ctx> ContextualDieRef<'ctx, VariableDie> {
    pub fn read_value_at_location(
        &self,
        type_decl: &TypeDeclaration,
        parent_fn: ContextualDieRef<FunctionDie>,
        pid: Pid,
    ) -> Option<Bytes> {
        self.die.location.as_ref().and_then(|loc| {
            let expr = loc.exprloc_value()?;
            let fb = weak_error!(parent_fn.frame_base_addr(pid))?;
            let eval_result = weak_error!(self
                .unit
                .evaluator(pid)
                .evaluate_with_opts(expr, EvalOption::new().with_base_frame(fb),))?;
            let bytes = weak_error!(eval_result.into_raw_buffer(type_decl.size_in_bytes(
                &EvaluationContext {
                    unit: self.unit,
                    pid,
                }
            )? as usize))?;
            Some(bytes)
        })
    }

    pub fn r#type(&self) -> Option<TypeDeclaration> {
        let entry = &self.context.deref_die(self.unit, self.die.type_ref?)?;
        let type_decl = match entry.die {
            DieVariant::BaseType(ref type_die) => TypeDeclaration::from(ContextualDieRef {
                context: self.context,
                unit: self.unit,
                node: &entry.node,
                die: type_die,
            }),
            DieVariant::StructType(ref type_die) => TypeDeclaration::from(ContextualDieRef {
                context: self.context,
                unit: self.unit,
                node: &entry.node,
                die: type_die,
            }),
            DieVariant::ArrayType(ref type_die) => TypeDeclaration::from(ContextualDieRef {
                context: self.context,
                unit: self.unit,
                node: &entry.node,
                die: type_die,
            }),
            DieVariant::EnumType(ref type_die) => TypeDeclaration::from(ContextualDieRef {
                context: self.context,
                unit: self.unit,
                node: &entry.node,
                die: type_die,
            }),
            DieVariant::PointerType(ref type_die) => TypeDeclaration::from(ContextualDieRef {
                context: self.context,
                unit: self.unit,
                node: &entry.node,
                die: type_die,
            }),
            _ => None?,
        };

        Some(type_decl)
    }

    pub fn valid_at(&self, pc: usize) -> bool {
        self.die
            .lexical_block_idx
            .map(|lb_idx| {
                let DieVariant::LexicalBlock(lb) = &self.unit.entries[lb_idx].die else {
                    unreachable!();
                };

                lb.ranges
                    .iter()
                    .any(|r| pc >= r.begin as usize && pc <= r.end as usize)
            })
            .unwrap_or(true)
    }
}
