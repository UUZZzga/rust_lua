use crate::objects::{Proto, TValue, UpVal, UpvalDesc, Instruction};

pub struct VmState {
    pub constants: Vec<TValue>,
    pub code: Vec<Instruction>,
    pub upval_descs: Vec<UpvalDesc>,
    pub protos: Vec<Proto>,
    pub base: usize,
    pub pc: usize,
    pub stack: Vec<TValue>,
    pub trap: bool,
    pub num_params: u8,
    pub is_vararg: bool,
    pub closure_upvals: Vec<UpVal>,
    pub open_upval: Option<usize>,
    pub tbc_list: Option<usize>,
    pub twups_linked: bool,
    pub is_in_twups: bool,
}

impl VmState {
    pub fn new(proto: &Proto, base: usize, stack: Vec<TValue>) -> Self {
        VmState {
            constants: proto.constants.clone(),
            code: proto.code.clone(),
            upval_descs: proto.upvalues.clone(),
            protos: proto.protos.clone(),
            base,
            pc: 0,
            stack,
            trap: false,
            num_params: proto.num_params,
            is_vararg: proto.is_vararg(),
            closure_upvals: Vec::new(),
            open_upval: None,
            tbc_list: None,
            twups_linked: false,
            is_in_twups: false,
        }
    }
}