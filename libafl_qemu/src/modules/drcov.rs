use std::{path::PathBuf, sync::Mutex};

use hashbrown::{hash_map::Entry, HashMap};
use libafl::{executors::ExitKind, observers::ObserversTuple, HasMetadata};
use libafl_qemu_sys::{GuestAddr, GuestUsize};
use libafl_targets::drcov::{DrCovBasicBlock, DrCovWriter};
use rangemap::RangeMap;
use serde::{Deserialize, Serialize};

use super::utils::filters::HasAddressFilter;
#[cfg(feature = "systemmode")]
use crate::modules::utils::filters::{NopPageFilter, NOP_PAGE_FILTER};
use crate::{
    emu::EmulatorModules,
    modules::{
        utils::filters::NopAddressFilter, AddressFilter, EmulatorModule, EmulatorModuleTuple,
    },
    qemu::Hook,
    Qemu,
};

static DRCOV_IDS: Mutex<Option<Vec<u64>>> = Mutex::new(None);
static DRCOV_MAP: Mutex<Option<HashMap<GuestAddr, u64>>> = Mutex::new(None);
static DRCOV_LENGTHS: Mutex<Option<HashMap<GuestAddr, GuestUsize>>> = Mutex::new(None);

#[cfg_attr(
    any(not(feature = "serdeany_autoreg"), miri),
    allow(clippy::unsafe_derive_deserialize)
)] // for SerdeAny
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DrCovMetadata {
    pub current_id: u64,
}

impl DrCovMetadata {
    #[must_use]
    pub fn new() -> Self {
        Self { current_id: 0 }
    }
}

libafl_bolts::impl_serdeany!(DrCovMetadata);

#[derive(Debug)]
pub struct DrCovModuleBuilder<F> {
    filter: Option<F>,
    module_mapping: Option<RangeMap<u64, (u16, String)>>,
    filename: Option<PathBuf>,
    full_trace: Option<bool>,
}

impl<F> DrCovModuleBuilder<F>
where
    F: AddressFilter,
{
    pub fn build(self) -> DrCovModule<F> {
        DrCovModule::new(
            self.filter.unwrap(),
            self.filename.unwrap(),
            self.module_mapping,
            self.full_trace.unwrap(),
        )
    }

    pub fn filter<F2>(self, filter: F2) -> DrCovModuleBuilder<F2> {
        DrCovModuleBuilder {
            filter: Some(filter),
            module_mapping: self.module_mapping,
            filename: self.filename,
            full_trace: self.full_trace,
        }
    }

    #[must_use]
    pub fn module_mapping(self, module_mapping: RangeMap<u64, (u16, String)>) -> Self {
        Self {
            filter: self.filter,
            module_mapping: Some(module_mapping),
            filename: self.filename,
            full_trace: self.full_trace,
        }
    }

    #[must_use]
    pub fn filename(self, filename: PathBuf) -> Self {
        Self {
            filter: self.filter,
            module_mapping: self.module_mapping,
            filename: Some(filename),
            full_trace: self.full_trace,
        }
    }

    #[must_use]
    pub fn full_trace(self, full_trace: bool) -> Self {
        Self {
            filter: self.filter,
            module_mapping: self.module_mapping,
            filename: self.filename,
            full_trace: Some(full_trace),
        }
    }
}

#[derive(Debug)]
pub struct DrCovModule<F> {
    filter: F,
    module_mapping: Option<RangeMap<u64, (u16, String)>>,
    filename: PathBuf,
    full_trace: bool,
    drcov_len: usize,
}

impl DrCovModule<NopAddressFilter> {
    #[must_use]
    pub fn builder() -> DrCovModuleBuilder<NopAddressFilter> {
        DrCovModuleBuilder {
            filter: Some(NopAddressFilter),
            module_mapping: None,
            full_trace: None,
            filename: None,
        }
    }
}
impl<F> DrCovModule<F> {
    #[must_use]
    #[expect(clippy::let_underscore_untyped)]
    pub fn new(
        filter: F,
        filename: PathBuf,
        module_mapping: Option<RangeMap<u64, (u16, String)>>,
        full_trace: bool,
    ) -> Self {
        if full_trace {
            let _ = DRCOV_IDS.lock().unwrap().insert(vec![]);
        }
        let _ = DRCOV_MAP.lock().unwrap().insert(HashMap::new());
        let _ = DRCOV_LENGTHS.lock().unwrap().insert(HashMap::new());
        Self {
            filter,
            module_mapping,
            filename,
            full_trace,
            drcov_len: 0,
        }
    }

    pub fn write(&mut self) {
        let lengths_opt = DRCOV_LENGTHS.lock().unwrap();
        let lengths = lengths_opt.as_ref().unwrap();
        if self.full_trace {
            if DRCOV_IDS.lock().unwrap().as_ref().unwrap().len() > self.drcov_len {
                let mut drcov_vec = Vec::<DrCovBasicBlock>::new();
                for id in DRCOV_IDS.lock().unwrap().as_ref().unwrap() {
                    'pcs_full: for (pc, idm) in DRCOV_MAP.lock().unwrap().as_ref().unwrap() {
                        let mut module_found = false;
                        // # Safety
                        //
                        // Module mapping is already set. It's checked or filled when the module is first run.
                        unsafe {
                            for module in self.module_mapping.as_ref().unwrap_unchecked().iter() {
                                let (range, (_, _)) = module;
                                if *pc >= range.start.try_into().unwrap()
                                    && *pc <= range.end.try_into().unwrap()
                                {
                                    module_found = true;
                                    break;
                                }
                            }
                        }
                        if !module_found {
                            continue 'pcs_full;
                        }
                        if *idm == *id {
                            #[expect(clippy::unnecessary_cast)] // for GuestAddr -> u64
                            match lengths.get(pc) {
                                Some(block_length) => {
                                    drcov_vec.push(DrCovBasicBlock::new(
                                        *pc as u64,
                                        *pc as u64 + *block_length as u64,
                                    ));
                                }
                                None => {
                                    log::info!("Failed to find block length for: {pc:}");
                                }
                            }
                        }
                    }
                }

                // # Safety
                //
                // Module mapping is already set. It's checked or filled when the module is first run.
                unsafe {
                    DrCovWriter::new(self.module_mapping.as_ref().unwrap_unchecked())
                        .write(&self.filename, &drcov_vec)
                        .expect("Failed to write coverage file");
                }
            }
            self.drcov_len = DRCOV_IDS.lock().unwrap().as_ref().unwrap().len();
        } else {
            if DRCOV_MAP.lock().unwrap().as_ref().unwrap().len() > self.drcov_len {
                let mut drcov_vec = Vec::<DrCovBasicBlock>::new();
                'pcs: for (pc, _) in DRCOV_MAP.lock().unwrap().as_ref().unwrap() {
                    let mut module_found = false;
                    // # Safety
                    //
                    // Module mapping is already set. It's checked or filled when the module is first run.
                    unsafe {
                        for module in self.module_mapping.as_ref().unwrap_unchecked().iter() {
                            let (range, (_, _)) = module;
                            if *pc >= range.start.try_into().unwrap()
                                && *pc <= range.end.try_into().unwrap()
                            {
                                module_found = true;
                                break;
                            }
                        }
                    }
                    if !module_found {
                        continue 'pcs;
                    }

                    #[expect(clippy::unnecessary_cast)] // for GuestAddr -> u64
                    match lengths.get(pc) {
                        Some(block_length) => {
                            drcov_vec.push(DrCovBasicBlock::new(
                                *pc as u64,
                                *pc as u64 + *block_length as u64,
                            ));
                        }
                        None => {
                            log::info!("Failed to find block length for: {pc:}");
                        }
                    }
                }

                // # Safety
                //
                // Module mapping is already set. It's checked or filled when the module is first run.
                unsafe {
                    DrCovWriter::new(self.module_mapping.as_ref().unwrap_unchecked())
                        .write(&self.filename, &drcov_vec)
                        .expect("Failed to write coverage file");
                }
            }
            self.drcov_len = DRCOV_MAP.lock().unwrap().as_ref().unwrap().len();
        }
    }
}

impl<F> DrCovModule<F>
where
    F: AddressFilter,
{
    #[must_use]
    pub fn must_instrument(&self, addr: GuestAddr) -> bool {
        self.filter.allowed(&addr)
    }
}

impl<F, I, S> EmulatorModule<I, S> for DrCovModule<F>
where
    F: AddressFilter,
    I: Unpin,
    S: Unpin + HasMetadata,
{
    fn post_qemu_init<ET>(&mut self, _qemu: Qemu, _emulator_modules: &mut EmulatorModules<ET, I, S>)
    where
        ET: EmulatorModuleTuple<I, S>,
    {
    }

    #[cfg(feature = "usermode")]
    fn first_exec<ET>(
        &mut self,
        qemu: Qemu,
        emulator_modules: &mut EmulatorModules<ET, I, S>,
        _state: &mut S,
    ) where
        ET: EmulatorModuleTuple<I, S>,
    {
        emulator_modules.blocks(
            Hook::Function(gen_unique_block_ids::<ET, F, I, S>),
            Hook::Function(gen_block_lengths::<ET, F, I, S>),
            Hook::Function(exec_trace_block::<ET, F, I, S>),
        );

        if self.module_mapping.is_none() {
            log::info!("Auto-filling module mapping for DrCov module from QEMU mapping.");

            let mut module_mapping: RangeMap<u64, (u16, String)> = RangeMap::new();

            #[expect(clippy::unnecessary_cast)] // for GuestAddr -> u64
            for (i, (r, p)) in qemu
                .mappings()
                .filter_map(|m| {
                    m.path()
                        .map(|p| ((m.start() as u64)..(m.end() as u64), p.to_string()))
                        .filter(|(_, p)| !p.is_empty())
                })
                .enumerate()
            {
                module_mapping.insert(r, (i as u16, p));
            }

            self.module_mapping = Some(module_mapping);
        } else {
            log::info!("Using user-provided module mapping for DrCov module.");
        }
    }

    #[cfg(feature = "systemmode")]
    fn first_exec<ET>(
        &mut self,
        _qemu: Qemu,
        _emulator_modules: &mut EmulatorModules<ET, I, S>,
        _state: &mut S,
    ) where
        ET: EmulatorModuleTuple<I, S>,
    {
        assert!(
            self.module_mapping.is_some(),
            "DrCov should have a module mapping already set."
        );
    }

    fn post_exec<OT, ET>(
        &mut self,
        _qemu: Qemu,
        _emulator_modules: &mut EmulatorModules<ET, I, S>,
        _state: &mut S,
        _input: &I,
        _observers: &mut OT,
        _exit_kind: &mut ExitKind,
    ) where
        OT: ObserversTuple<I, S>,
        ET: EmulatorModuleTuple<I, S>,
    {
        self.write();
    }

    unsafe fn on_crash(&mut self) {
        self.write();
    }

    unsafe fn on_timeout(&mut self) {
        self.write();
    }
}

impl<F> HasAddressFilter for DrCovModule<F>
where
    F: AddressFilter,
{
    type ModuleAddressFilter = F;
    #[cfg(feature = "systemmode")]
    type ModulePageFilter = NopPageFilter;

    fn address_filter(&self) -> &Self::ModuleAddressFilter {
        &self.filter
    }

    fn address_filter_mut(&mut self) -> &mut Self::ModuleAddressFilter {
        &mut self.filter
    }

    #[cfg(feature = "systemmode")]
    fn page_filter(&self) -> &Self::ModulePageFilter {
        &NopPageFilter
    }

    #[cfg(feature = "systemmode")]
    fn page_filter_mut(&mut self) -> &mut Self::ModulePageFilter {
        unsafe { (&raw mut NOP_PAGE_FILTER).as_mut().unwrap().get_mut() }
    }
}

pub fn gen_unique_block_ids<ET, F, I, S>(
    _qemu: Qemu,
    emulator_modules: &mut EmulatorModules<ET, I, S>,
    state: Option<&mut S>,
    pc: GuestAddr,
) -> Option<u64>
where
    ET: EmulatorModuleTuple<I, S>,
    F: AddressFilter,
    I: Unpin,
    S: Unpin + HasMetadata,
{
    let drcov_module = emulator_modules.get::<DrCovModule<F>>().unwrap();
    if !drcov_module.must_instrument(pc) {
        return None;
    }

    let state = state.expect("The gen_unique_block_ids hook works only for in-process fuzzing. Is the Executor initialized?");
    if state
        .metadata_map_mut()
        .get_mut::<DrCovMetadata>()
        .is_none()
    {
        state.add_metadata(DrCovMetadata::new());
    }
    let meta = state.metadata_map_mut().get_mut::<DrCovMetadata>().unwrap();

    match DRCOV_MAP.lock().unwrap().as_mut().unwrap().entry(pc) {
        Entry::Occupied(e) => {
            let id = *e.get();
            if drcov_module.full_trace {
                Some(id)
            } else {
                None
            }
        }
        Entry::Vacant(e) => {
            let id = meta.current_id;
            e.insert(id);
            meta.current_id = id + 1;
            if drcov_module.full_trace {
                // GuestAddress is u32 for 32 bit guests
                #[expect(clippy::unnecessary_cast)]
                Some(id as u64)
            } else {
                None
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // no longer a problem with nightly
pub fn gen_block_lengths<ET, F, I, S>(
    _qemu: Qemu,
    emulator_modules: &mut EmulatorModules<ET, I, S>,
    _state: Option<&mut S>,
    pc: GuestAddr,
    block_length: GuestUsize,
) where
    ET: EmulatorModuleTuple<I, S>,
    F: AddressFilter,
    I: Unpin,
    S: Unpin + HasMetadata,
{
    let drcov_module = emulator_modules.get::<DrCovModule<F>>().unwrap();
    if !drcov_module.must_instrument(pc) {
        return;
    }
    DRCOV_LENGTHS
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .insert(pc, block_length);
}

#[allow(clippy::needless_pass_by_value)] // no longer a problem with nightly
pub fn exec_trace_block<ET, F, I, S>(
    _qemu: Qemu,
    emulator_modules: &mut EmulatorModules<ET, I, S>,
    _state: Option<&mut S>,
    id: u64,
) where
    ET: EmulatorModuleTuple<I, S>,
    F: AddressFilter,
    I: Unpin,
    S: Unpin + HasMetadata,
{
    if emulator_modules.get::<DrCovModule<F>>().unwrap().full_trace {
        DRCOV_IDS.lock().unwrap().as_mut().unwrap().push(id);
    }
}
