use crate::debugger::address::RelocatedAddress;
use crate::debugger::debugee::dwarf::eval::ExpressionEvaluator;
use crate::debugger::debugee::dwarf::EndianRcSlice;
use crate::debugger::debugee::{Debugee, Location};
use crate::debugger::register::{DwarfRegisterMap, RegisterMap};
use crate::debugger::utils::TryGetOrInsert;
use crate::{debugger, weak_error};
use anyhow::anyhow;
use gimli::{EhFrame, FrameDescriptionEntry, RegisterRule, UnwindSection};
use std::mem;

/// Represents information about single stack frame in unwind path.
#[derive(Debug)]
pub struct FrameSpan {
    pub func_name: Option<String>,
    pub fn_start_ip: Option<RelocatedAddress>,
    pub ip: RelocatedAddress,
}

/// UnwindContext contains information for unwinding single frame.  
pub struct UnwindContext<'a> {
    registers: DwarfRegisterMap,
    location: Location,
    fde: FrameDescriptionEntry<EndianRcSlice, usize>,
    debugee: &'a Debugee,
    cfa: RelocatedAddress,
}

impl<'a> UnwindContext<'a> {
    fn new(
        debugee: &'a Debugee,
        registers: DwarfRegisterMap,
        location: Location,
    ) -> anyhow::Result<Option<Self>> {
        let dwarf = &debugee.dwarf;
        let mut next_registers = registers.clone();
        let registers_snap = registers;
        let fde = match dwarf.eh_frame.fde_for_address(
            &dwarf.bases,
            location.global_pc.into(),
            EhFrame::cie_from_offset,
        ) {
            Ok(fde) => fde,
            Err(gimli::Error::NoUnwindInfoForAddress) => {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };

        let mut ctx = Box::new(gimli::UnwindContext::new());
        let row = fde.unwind_info_for_address(
            &dwarf.eh_frame,
            &dwarf.bases,
            &mut ctx,
            location.global_pc.into(),
        )?;
        let cfa = dwarf.evaluate_cfa(debugee, &registers_snap, row, location)?;

        let mut lazy_evaluator = None;
        let evaluator_init_fn = || -> anyhow::Result<ExpressionEvaluator> {
            let unit = dwarf
                .find_unit_by_pc(location.global_pc)
                .ok_or_else(|| anyhow!("undefined unit"))?;
            Ok(unit.evaluator(debugee))
        };

        row.registers()
            .filter_map(|(register, rule)| {
                let value = match rule {
                    RegisterRule::Undefined => return None,
                    RegisterRule::SameValue => {
                        let register_map = weak_error!(RegisterMap::current(location.pid))?;
                        weak_error!(DwarfRegisterMap::from(register_map).value(*register))?
                    }
                    RegisterRule::Offset(offset) => {
                        let addr =
                            RelocatedAddress::from(usize::from(cfa).wrapping_add(*offset as usize));

                        let bytes = weak_error!(debugger::read_memory_by_pid(
                            location.pid,
                            addr.into(),
                            mem::size_of::<u64>()
                        ))?;
                        u64::from_ne_bytes(weak_error!(bytes
                            .try_into()
                            .map_err(|e| anyhow!("{e:?}")))?)
                    }
                    RegisterRule::ValOffset(offset) => cfa.offset(*offset as isize).into(),
                    RegisterRule::Register(reg) => weak_error!(registers_snap.value(*reg))?,
                    RegisterRule::Expression(expr) => {
                        let evaluator =
                            weak_error!(lazy_evaluator.try_get_or_insert_with(evaluator_init_fn))?;
                        let expr_result =
                            weak_error!(evaluator.evaluate(location.pid, expr.clone()))?;
                        let addr = weak_error!(expr_result.into_scalar::<usize>())?;
                        let bytes = weak_error!(debugger::read_memory_by_pid(
                            location.pid,
                            addr,
                            mem::size_of::<u64>()
                        ))?;
                        u64::from_ne_bytes(weak_error!(bytes
                            .try_into()
                            .map_err(|e| anyhow!("{e:?}")))?)
                    }
                    RegisterRule::ValExpression(expr) => {
                        let evaluator =
                            weak_error!(lazy_evaluator.try_get_or_insert_with(evaluator_init_fn))?;
                        let expr_result =
                            weak_error!(evaluator.evaluate(location.pid, expr.clone()))?;
                        weak_error!(expr_result.into_scalar::<u64>())?
                    }
                    RegisterRule::Architectural => return None,
                };

                Some((*register, value))
            })
            .for_each(|(reg, val)| next_registers.update(reg, val));

        Ok(Some(Self {
            registers: next_registers,
            location,
            debugee,
            fde,
            cfa,
        }))
    }

    fn next(previous_ctx: UnwindContext<'a>, location: Location) -> anyhow::Result<Option<Self>> {
        let mut next_frame_registers: DwarfRegisterMap = previous_ctx.registers;
        next_frame_registers.update(gimli::Register(7), previous_ctx.cfa.into());
        UnwindContext::new(previous_ctx.debugee, next_frame_registers, location)
    }

    fn return_address(&self) -> Option<RelocatedAddress> {
        let register = self.fde.cie().return_address_register();
        self.registers
            .value(register)
            .map(RelocatedAddress::from)
            .ok()
    }

    pub fn registers(&self) -> DwarfRegisterMap {
        self.registers.clone()
    }
}

/// Unwind debugee call stack by dwarf information.
///
/// `DwarfUnwinder` also useful for getting return address for current location and register values for subroutine entry.
///
/// Currently this application using `unwind::libunwind` module for stack unwinding.
/// Main reason of it that `DwarfUnwinder` knows locations information about which is in the `eh_frame` section of elf file.
/// But not all possible locations can be found in `eh_frame`, and for this locations unwinding may fail.
/// For example one of threads may be in syscall when we want to unwind his stack.
/// Libunwind is more generic approach because it relies on details specific to specific architectures,
/// and this why `DwarfUnwinder` is unused in stack unwinding case.
/// Nevertheless `DwarfUnwinder` may be useful for getting return address and register values.
pub struct DwarfUnwinder<'a> {
    debugee: &'a Debugee,
}

impl<'a> DwarfUnwinder<'a> {
    /// Creates new unwinder.
    ///
    /// # Arguments
    ///
    /// * `debugee`: current debugee program.
    pub fn new(debugee: &'a Debugee) -> DwarfUnwinder {
        Self { debugee }
    }

    /// Unwind call stack.
    ///
    /// # Arguments
    ///
    /// * `location`: position information about instruction pointer and thread where unwind start from.
    pub fn unwind(&self, location: Location) -> anyhow::Result<Vec<FrameSpan>> {
        let mb_unwind_ctx = UnwindContext::new(
            self.debugee,
            DwarfRegisterMap::from(RegisterMap::current(location.pid)?),
            location,
        )?;
        let Some(mut unwind_ctx) = mb_unwind_ctx else {
            return Ok(vec![]);
        };

        let function = self.debugee.dwarf.find_function_by_pc(location.global_pc);
        let fn_start_at = function.and_then(|func| {
            func.prolog_start_place()
                .ok()
                .map(|prolog| prolog.address.relocate(self.debugee.mapping_offset()))
        });

        let mut bt = vec![FrameSpan {
            func_name: function.and_then(|func| func.full_name()),
            fn_start_ip: fn_start_at,
            ip: location.pc,
        }];

        // start unwind
        while let Some(return_addr) = unwind_ctx.return_address() {
            let next_location = Location {
                pc: return_addr,
                global_pc: return_addr.into_global(self.debugee.mapping_offset()),
                pid: unwind_ctx.location.pid,
            };

            unwind_ctx = match UnwindContext::next(unwind_ctx, next_location)? {
                None => break,
                Some(ctx) => ctx,
            };

            let function = self
                .debugee
                .dwarf
                .find_function_by_pc(next_location.global_pc);
            let fn_start_at = function.and_then(|func| {
                func.prolog_start_place()
                    .ok()
                    .map(|prolog| prolog.address.relocate(self.debugee.mapping_offset()))
            });

            let span = FrameSpan {
                func_name: function.and_then(|func| func.full_name()),
                fn_start_ip: fn_start_at,
                ip: next_location.pc,
            };
            bt.push(span);
        }

        Ok(bt)
    }

    /// Returns return address for function determine by location.
    ///
    /// # Arguments
    ///
    /// * `location`: some debugee thread position.
    pub fn return_address(&self, location: Location) -> anyhow::Result<Option<RelocatedAddress>> {
        let mb_unwind_ctx = UnwindContext::new(
            self.debugee,
            DwarfRegisterMap::from(RegisterMap::current(location.pid)?),
            location,
        )?;

        if let Some(unwind_ctx) = mb_unwind_ctx {
            return Ok(unwind_ctx.return_address());
        }
        Ok(None)
    }

    /// Returns unwind context for location.
    ///
    /// # Arguments
    ///
    /// * `location`: some debugee thread position.
    pub fn context_for(&self, location: Location) -> anyhow::Result<Option<UnwindContext>> {
        UnwindContext::new(
            self.debugee,
            DwarfRegisterMap::from(RegisterMap::current(location.pid)?),
            location,
        )
    }
}

pub mod libunwind {
    use super::FrameSpan;
    use crate::debugger::address::RelocatedAddress;
    use nix::unistd::Pid;
    use unwind::{Accessors, AddressSpace, Byteorder, Cursor, PTraceState, RegNum};

    pub type Backtrace = Vec<FrameSpan>;

    /// Unwind thread stack and returns backtrace.
    ///
    /// # Arguments
    ///
    /// * `pid`: thread for unwinding.
    pub fn unwind(pid: Pid) -> unwind::Result<Backtrace> {
        let state = PTraceState::new(pid.as_raw() as u32)?;
        let address_space = AddressSpace::new(Accessors::ptrace(), Byteorder::DEFAULT)?;
        let mut cursor = Cursor::remote(&address_space, &state)?;
        let mut backtrace = vec![];

        loop {
            let ip = cursor.register(RegNum::IP)?;
            match (cursor.procedure_info(), cursor.procedure_name()) {
                (Ok(ref info), Ok(ref name)) if ip == info.start_ip() + name.offset() => {
                    let fn_name = format!("{:#}", rustc_demangle::demangle(name.name()));

                    backtrace.push(FrameSpan {
                        func_name: Some(fn_name),
                        fn_start_ip: Some(info.start_ip().into()),
                        ip: ip.into(),
                    });
                }
                _ => {
                    backtrace.push(FrameSpan {
                        func_name: None,
                        fn_start_ip: None,
                        ip: ip.into(),
                    });
                }
            }

            if !cursor.step()? {
                break;
            }
        }

        Ok(backtrace)
    }

    /// Returns return address for stopped thread.
    ///
    /// # Arguments
    ///
    /// * `pid`: pid of stopped thread.
    pub fn return_addr(pid: Pid) -> unwind::Result<Option<RelocatedAddress>> {
        let state = PTraceState::new(pid.as_raw() as u32)?;
        let address_space = AddressSpace::new(Accessors::ptrace(), Byteorder::DEFAULT)?;
        let mut cursor = Cursor::remote(&address_space, &state)?;

        if !cursor.step()? {
            return Ok(None);
        }

        Ok(Some(RelocatedAddress::from(cursor.register(RegNum::IP)?)))
    }
}