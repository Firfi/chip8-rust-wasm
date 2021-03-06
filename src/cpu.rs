use std::{println};
use std::borrow::BorrowMut;
use std::convert::{TryFrom};
use std::future::Future;
use std::ops::{Add, DerefMut, Sub};
use std::sync::{Arc, Mutex};

use std::time::Duration;
use anyhow::Result;
use fluvio_wasm_timer::{Delay};
use futures::future::BoxFuture;
use crate::keyboard::PC_KEY_MAP;


use rand::{Rng, RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use ux::{u12, u4};


use crate::cpu_decoder::{decode};
use crate::macros::newtype_copy;
use crate::screen::{Screen};
use crate::keyboard::{KeyboardState};

const MEM_SIZE: usize = 4096;
const PROGRAM_START_ADDR: u16 = 0x0200;
const STACK_SIZE: usize = 16;
const REGISTERS_SIZE: usize = 16;

const STEPS_PER_CYCLE: usize = 10;
const SPEED: u64 = 60; // herz

const FONTS_LENGTH: usize = 80;
const FONTS: [MemPrimitive; FONTS_LENGTH] = [
    0xF0, 0x90, 0x90, 0x90, 0xF0, // 0
    0x20, 0x60, 0x20, 0x20, 0x70, // 1
    0xF0, 0x10, 0xF0, 0x80, 0xF0, // 2
    0xF0, 0x10, 0xF0, 0x10, 0xF0, // 3
    0x90, 0x90, 0xF0, 0x10, 0x10, // 4
    0xF0, 0x80, 0xF0, 0x10, 0xF0, // 5
    0xF0, 0x80, 0xF0, 0x90, 0xF0, // 6
    0xF0, 0x10, 0x20, 0x40, 0x40, // 7
    0xF0, 0x90, 0xF0, 0x90, 0xF0, // 8
    0xF0, 0x90, 0xF0, 0x10, 0xF0, // 9
    0xF0, 0x90, 0xF0, 0x90, 0x90, // A
    0xE0, 0x90, 0xE0, 0x90, 0xE0, // B
    0xF0, 0x80, 0x80, 0x80, 0xF0, // C
    0xE0, 0x90, 0x90, 0x90, 0xE0, // D
    0xF0, 0x80, 0xF0, 0x80, 0xF0, // E
    0xF0, 0x80, 0xF0, 0x80, 0x80  // F
];
type MemPrimitive = u8;
#[derive(Clone, Debug, Copy)]
pub(crate) struct MemValue(pub(crate) MemPrimitive);
// newtype_copy!(MemValue);

pub(crate) type Mem = [MemValue; MEM_SIZE];
#[derive(Clone, Debug)]
pub(crate) struct PC(pub(crate) u12);
#[derive(Clone, Debug)]
pub(crate) struct SP(pub(crate) u4);
#[derive(Clone, Debug)]
pub(crate) struct I(pub(crate) u12);
#[derive(Debug)]
pub(crate) struct V(pub(crate) MemPrimitive);
newtype_copy!(V);
#[derive(Clone, Debug)]
pub(crate) struct DT(pub(crate) MemPrimitive);
#[derive(Clone, Debug)]
pub(crate) struct ST(pub(crate) MemPrimitive);
#[derive(Clone, Debug)]
pub(crate) struct Repaint(pub(crate) bool);
#[derive(Clone, Debug)]
pub(crate) struct Halted(pub(crate) bool);
#[derive(Clone, Debug)]
pub(crate) struct WaitingKb(pub(crate) bool);

#[derive(Clone, Debug)]
pub struct CPUState {
    pub(crate) mem: Mem,
    /*
    16 8-bit (one byte) general-purpose variable registers numbered 0 through F hexadecimal, ie. 0 through 15 in decimal, called V0 through `VF
    VF is also used as a flag register; many instructions will set it to either 1 or 0 based on some rule, for example using it as a carry flag
     */
    pub(crate) v: [V; REGISTERS_SIZE],
    pub(crate) pc: PC,
    /*
    Both the index register, program counter and stack entries are actually 16 bits long.
     In theory, they could increment beyond 4 kB of memory addresses.
     In practice, no CHIP-8 games do that.
     The early computers running CHIP-8 usually had less than 4 kB of RAM anyway.
     */
    pub(crate) i: I,
    pub(crate) stack: [u12; STACK_SIZE],
    pub(crate) sp: SP,
    pub(crate) repaint: Repaint,
    pub(crate) halted: Halted,
    pub(crate) waiting_kb: WaitingKb,
    pub(crate) waiting_kb_x: Option<X>,
    // timers
    pub(crate) dt: DT,
    pub(crate) st: ST,
    pub(crate) quirks: CPUQuirks,
    // not in spec
    pub(crate) rng_seed: u64,
    pub(crate) keyboard: KeyboardState,
}

/**
* Enables/disabled CPU quirks
* @property {boolean} shift - If enabled, VX is shifted and VY remains unchanged (default: false)
* @property {boolean} loadStore - If enabled, I is not incremented during load/store (default: false)
*/
#[derive(Clone, Debug)]
pub(crate) struct CPUQuirks {
    pub(crate) shift: bool,
    pub(crate) load_store: bool,
}

impl CPUQuirks {
    pub fn new() -> Self {
        CPUQuirks { shift: false, load_store: false }
    }
}

impl CPUState {
    fn fetch(&self) -> u16 {
        u16::from_be_bytes([self.mem[self.pci()].0, self.mem[self.pci() + 1].0])
    }
    pub(crate) fn pci(&self) -> usize {
        let r: u16 = self.pc.0.into();
        r.into()
    }
    pub(crate) fn update_timers(&mut self) {
        if self.dt.0 > 0 {
            self.dt.0 -= 1;
        }
        if self.st.0 > 0 {
            self.st.0 -= 1;
        }
    }
    pub(crate) fn run_rng(&mut self) -> u8 { // 0..255
        let mut rng = ChaCha8Rng::seed_from_u64(self.rng_seed);
        self.rng_seed = rng.next_u64();
        rng.gen()
    }
    // no more incs planned never; 2 fns is fine
    pub(crate) fn inc_pc_2(&mut self) {
        self.pc.0 = self.pc.0.add(u12::new(2));
    }
    // fn inc_pc_4(&mut self) {
    //     self.pc = self.pc.add(u12::new(4));
    // }
}

use wasm_bindgen::prelude::*;
use crate::cpu_instructions::X;

pub struct CPU {
    pub(crate) state: CPUState,
    stopped: bool,
    screen: Box<dyn Screen>,
}

fn load_font_set(mem: &mut Mem) {
    for i in 0..FONTS_LENGTH {
        mem[i].0 = FONTS[i];
    }
}

impl CPU {
    pub fn new(screen: Box<dyn Screen>) -> Self {
        let mut mem = [MemValue(0); MEM_SIZE];
        load_font_set(&mut mem);
        Self {
            state: CPUState {
                mem,
                v: [V(0); REGISTERS_SIZE],
                pc: PC(u12::new(PROGRAM_START_ADDR)),
                i: I(u12::new(0)),
                stack: [u12::new(0); STACK_SIZE],
                sp: SP(u4::new(0)),
                repaint: Repaint(false),
                halted: Halted(false),
                waiting_kb: WaitingKb(false),
                waiting_kb_x: None,
                dt: DT(0),
                st: ST(0),
                quirks: CPUQuirks::new(),
                rng_seed: rand::thread_rng().next_u64(),
                keyboard: KeyboardState::new(),
            },
            screen,
            stopped: false,
        }
    }
    pub fn load_program(&mut self, data: Vec<u8>) {
        assert!(u12::max_value().sub(u12::new(PROGRAM_START_ADDR)) >= u12::new(u16::try_from(data.len()).expect("Data len takes more than u16")));
        for (i, x) in data.iter().enumerate() {
            self.state.mem[usize::from(PROGRAM_START_ADDR) + i].0 = *x;
        }
    }

    // for spawning locally
    // wasm compatible - awaits to free the thread instead of blocking
    // todo maybe get delay constructor as an argument
    // TODO do something with wasm_mutex::Mutex requirement\
    pub async fn run(this: Arc<wasm_mutex::Mutex<CPU>>) {
        let arc = this.clone();
        loop {
            Delay::new(Duration::new(1 / SPEED, 0)).await;
            let mut guard = arc.lock().await;
            let guard: &mut CPU = guard.deref_mut(); // for compiler to understand splitting borrow later
            if guard.is_done() {
                break;
            }
            // TODO locks for too long?
            guard.screen.request_animation_frame().await;
            match CPU::cycle(&mut guard.state, &mut *guard.screen) {
                Ok(()) => (),
                Err(e) => {
                    println!("Error during cycle, {}. STOPPING", e);
                    guard.stop();
                }
            }
        }
    }

    pub fn is_done(&self) -> bool {
        self.stopped
    }

    pub fn is_paused(&self) -> bool {
        self.state.halted.0
    }

    pub fn stop(&mut self) {
        self.stopped = true;
    }

    fn cycle(state: &mut CPUState, screen_draw: &mut dyn Screen) -> Result<()> {
        if state.halted.0 {
            return Ok(());
        }
        for _ in 0..STEPS_PER_CYCLE {
            CPU::step(state, screen_draw)?;
        }
        state.update_timers();
        Ok(())
        // if (this.st > 0) {
        //     this.audio.play();
        // } else {
        //     this.audio.stop();
        // }
    }

    pub(crate) fn step(state: &mut CPUState, screen_draw: &mut dyn Screen) -> StepResult {
        let opcode = state.fetch();
        let op = decode(opcode)?;
        // TODO result type, error type
        CPU::execute(state, screen_draw, op);
        if state.repaint.0 {
            screen_draw.repaint();
            state.repaint.0 = false;
        }
        StepResult::Ok(())
    }

    fn execute(state: &mut CPUState, screen_draw: &mut dyn Screen, op: impl Fn(&mut CPUState, &mut dyn Screen)) {
        op(state, screen_draw);
    }

    pub fn key_down(&mut self, kbk: usize) {
        
        let k = PC_KEY_MAP.get(&kbk);
        if k.is_none() {
            return;
        }
        let k = k.unwrap();
        self.state.keyboard.key_down(k);
        // todo or keyup?
        if self.state.waiting_kb.0 {
            let x = self.state.waiting_kb_x.as_ref().expect("waiting for kb but no X");
            self.state.v[x.0] = V(b'6');
            self.state.inc_pc_2();
            self.state.halted.0 = false;
            self.state.waiting_kb.0 = false;
            self.state.waiting_kb_x = None;

        }
    }

    pub fn key_up(&mut self, k: usize) {
        let k = PC_KEY_MAP.get(&k);
        if k.is_none() {
            return;
        }
        let k = k.unwrap();
        self.state.keyboard.key_up(k);
    }

}

type StepResult = Result<()>;