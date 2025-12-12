use rand::{Rng, SeedableRng, rngs::SmallRng};
use std::time::{Duration, Instant};

pub const MEM_SIZE: usize = 4096;
pub const SCREEN_W: usize = 64;
pub const SCREEN_H: usize = 32;

const FONT_BASE: usize = 0x50;
const PROGRAM_START: u16 = 0x200;

#[derive(Debug, Clone, Copy)]
pub struct StepResult {
    pub draw: bool,
    pub waiting_for_key: bool,
}

pub struct Chip8 {
    pub mem: [u8; MEM_SIZE],
    pub v: [u8; 16],
    pub i: u16,
    pub pc: u16,
    pub sp: u8,
    pub stack: [u16; 16],

    pub delay: u8,
    pub sound: u8,

    pub display: [u8; SCREEN_W * SCREEN_H], // 0/1 pixels
    pub keys: [bool; 16],

    waiting_reg: Option<usize>,

    rng: SmallRng,
    last_timer_tick: Instant,
}

impl Chip8 {
    pub fn new() -> Self {
        let mut s = Self {
            mem: [0; MEM_SIZE],
            v: [0; 16],
            i: 0,
            pc: PROGRAM_START,
            sp: 0,
            stack: [0; 16],
            delay: 0,
            sound: 0,
            display: [0; SCREEN_W * SCREEN_H],
            keys: [false; 16],
            waiting_reg: None,
            rng: SmallRng::from_entropy(),
            last_timer_tick: Instant::now(),
        };
        s.load_font();
        s
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn load_rom(&mut self, rom: &[u8]) -> Result<(), &'static str> {
        let start = PROGRAM_START as usize;
        if start + rom.len() > MEM_SIZE {
            return Err("ROM too large");
        }
        self.mem[start..start + rom.len()].copy_from_slice(rom);
        self.pc = PROGRAM_START;
        Ok(())
    }

    pub fn set_key(&mut self, key: usize, down: bool) {
        if key < 16 {
            self.keys[key] = down;
            // If we were waiting for a key press, accept the first pressed key.
            if down {
                if let Some(reg) = self.waiting_reg.take() {
                    self.v[reg] = key as u8;
                    self.pc = self.pc.wrapping_add(2);
                }
            }
        }
    }

    pub fn render_ansi(&self) -> String {
        // Clear + home, then 32 lines of 64 chars.
        let mut out = String::from("\x1b[2J\x1b[H");
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                out.push(if self.display[y * SCREEN_W + x] != 0 {
                    '#'
                } else {
                    '.'
                });
            }
            out.push('\n');
        }
        out
    }

    pub fn step(&mut self, opcode: u16) -> StepResult {
        self.tick_timers();

        // If we are waiting for key (Fx0A), ignore execution until a key press comes in via SET_KEY.
        if self.waiting_reg.is_some() {
            return StepResult {
                draw: false,
                waiting_for_key: true,
            };
        }

        let nnn = opcode & 0x0FFF;
        let kk = (opcode & 0x00FF) as u8;
        let x = ((opcode & 0x0F00) >> 8) as usize;
        let y = ((opcode & 0x00F0) >> 4) as usize;
        let n = (opcode & 0x000F) as u8;

        let mut draw = false;

        match opcode & 0xF000 {
            0x0000 => match opcode {
                0x00E0 => {
                    self.display.fill(0);
                    self.pc = self.pc.wrapping_add(2);
                    draw = true;
                }
                0x00EE => {
                    if self.sp == 0 {
                        // Underflow => ignore safely
                        self.pc = self.pc.wrapping_add(2);
                    } else {
                        self.sp -= 1;
                        self.pc = self.stack[self.sp as usize];
                        self.pc = self.pc.wrapping_add(2);
                    }
                }
                _ => {
                    // 0NNN (SYS) ignored on modern interpreters
                    self.pc = self.pc.wrapping_add(2);
                }
            },

            0x1000 => self.pc = nnn,
            0x2000 => {
                if (self.sp as usize) < self.stack.len() {
                    self.stack[self.sp as usize] = self.pc;
                    self.sp += 1;
                    self.pc = nnn;
                } else {
                    // Stack overflow => ignore safely
                    self.pc = self.pc.wrapping_add(2);
                }
            }

            0x3000 => {
                self.pc = self.pc.wrapping_add(if self.v[x] == kk { 4 } else { 2 });
            }
            0x4000 => {
                self.pc = self.pc.wrapping_add(if self.v[x] != kk { 4 } else { 2 });
            }
            0x5000 => {
                if (opcode & 0x000F) == 0 {
                    self.pc = self
                        .pc
                        .wrapping_add(if self.v[x] == self.v[y] { 4 } else { 2 });
                } else {
                    self.pc = self.pc.wrapping_add(2);
                }
            }

            0x6000 => {
                self.v[x] = kk;
                self.pc = self.pc.wrapping_add(2);
            }
            0x7000 => {
                self.v[x] = self.v[x].wrapping_add(kk);
                self.pc = self.pc.wrapping_add(2);
            }

            0x8000 => {
                match opcode & 0x000F {
                    0x0 => self.v[x] = self.v[y],
                    0x1 => self.v[x] |= self.v[y],
                    0x2 => self.v[x] &= self.v[y],
                    0x3 => self.v[x] ^= self.v[y],
                    0x4 => {
                        let (res, carry) = self.v[x].overflowing_add(self.v[y]);
                        self.v[x] = res;
                        self.v[0xF] = if carry { 1 } else { 0 };
                    }
                    0x5 => {
                        let (res, borrow) = self.v[x].overflowing_sub(self.v[y]);
                        self.v[x] = res;
                        self.v[0xF] = if borrow { 0 } else { 1 };
                    }
                    0x6 => {
                        // Modern: shift VX
                        self.v[0xF] = self.v[x] & 1;
                        self.v[x] >>= 1;
                    }
                    0x7 => {
                        let (res, borrow) = self.v[y].overflowing_sub(self.v[x]);
                        self.v[x] = res;
                        self.v[0xF] = if borrow { 0 } else { 1 };
                    }
                    0xE => {
                        self.v[0xF] = (self.v[x] >> 7) & 1;
                        self.v[x] <<= 1;
                    }
                    _ => {}
                }
                self.pc = self.pc.wrapping_add(2);
            }

            0x9000 => {
                if (opcode & 0x000F) == 0 {
                    self.pc = self
                        .pc
                        .wrapping_add(if self.v[x] != self.v[y] { 4 } else { 2 });
                } else {
                    self.pc = self.pc.wrapping_add(2);
                }
            }

            0xA000 => {
                self.i = nnn;
                self.pc = self.pc.wrapping_add(2);
            }
            0xB000 => {
                self.pc = nnn.wrapping_add(self.v[0] as u16);
            }
            0xC000 => {
                let r: u8 = self.rng.r#gen();
                self.v[x] = r & kk;
                self.pc = self.pc.wrapping_add(2);
            }
            0xD000 => {
                // Draw N-byte sprite from mem[I..I+N] at (VX, VY)
                let vx = self.v[x] as usize;
                let vy = self.v[y] as usize;
                self.v[0xF] = 0;

                for row in 0..(n as usize) {
                    let addr = self.i as usize + row;
                    if addr >= MEM_SIZE {
                        break;
                    }
                    let sprite = self.mem[addr];
                    for bit in 0..8 {
                        let px = (vx + bit) % SCREEN_W;
                        let py = (vy + row) % SCREEN_H;
                        let idx = py * SCREEN_W + px;

                        let sprite_bit = (sprite >> (7 - bit)) & 1;
                        if sprite_bit == 1 {
                            if self.display[idx] == 1 {
                                self.v[0xF] = 1;
                            }
                            self.display[idx] ^= 1;
                        }
                    }
                }

                self.pc = self.pc.wrapping_add(2);
                draw = true;
            }

            0xE000 => match opcode & 0x00FF {
                0x9E => {
                    let key = (self.v[x] & 0x0F) as usize;
                    self.pc = self.pc.wrapping_add(if self.keys[key] { 4 } else { 2 });
                }
                0xA1 => {
                    let key = (self.v[x] & 0x0F) as usize;
                    self.pc = self.pc.wrapping_add(if !self.keys[key] { 4 } else { 2 });
                }
                _ => self.pc = self.pc.wrapping_add(2),
            },

            0xF000 => match opcode & 0x00FF {
                0x07 => {
                    self.v[x] = self.delay;
                    self.pc = self.pc.wrapping_add(2);
                }
                0x0A => {
                    // Wait for key press; do not advance PC until key arrives.
                    self.waiting_reg = Some(x);
                }
                0x15 => {
                    self.delay = self.v[x];
                    self.pc = self.pc.wrapping_add(2);
                }
                0x18 => {
                    self.sound = self.v[x];
                    self.pc = self.pc.wrapping_add(2);
                }
                0x1E => {
                    self.i = self.i.wrapping_add(self.v[x] as u16);
                    self.pc = self.pc.wrapping_add(2);
                }
                0x29 => {
                    // Font sprite for hex digit VX (0..F)
                    let digit = (self.v[x] & 0x0F) as usize;
                    self.i = (FONT_BASE + digit * 5) as u16;
                    self.pc = self.pc.wrapping_add(2);
                }
                0x33 => {
                    // BCD of VX into mem[I..I+2]
                    let vx = self.v[x];
                    let a = self.i as usize;
                    if a + 2 < MEM_SIZE {
                        self.mem[a] = vx / 100;
                        self.mem[a + 1] = (vx / 10) % 10;
                        self.mem[a + 2] = vx % 10;
                    }
                    self.pc = self.pc.wrapping_add(2);
                }
                0x55 => {
                    // Store V0..VX at mem[I..]
                    let a = self.i as usize;
                    if a < MEM_SIZE {
                        let end = (a + x + 1).min(MEM_SIZE);
                        for (idx, dst) in (a..end).enumerate() {
                            self.mem[dst] = self.v[idx];
                        }
                    }
                    self.pc = self.pc.wrapping_add(2);
                }
                0x65 => {
                    // Load V0..VX from mem[I..]
                    let a = self.i as usize;
                    if a < MEM_SIZE {
                        let end = (a + x + 1).min(MEM_SIZE);
                        for (idx, src) in (a..end).enumerate() {
                            self.v[idx] = self.mem[src];
                        }
                    }
                    self.pc = self.pc.wrapping_add(2);
                }
                _ => {
                    self.pc = self.pc.wrapping_add(2);
                }
            },

            _ => self.pc = self.pc.wrapping_add(2),
        }

        StepResult {
            draw,
            waiting_for_key: false,
        }
    }

    fn tick_timers(&mut self) {
        // Rough 60Hz timer tick based on wall time.
        let now = Instant::now();
        let mut elapsed = now.duration_since(self.last_timer_tick);

        // Tick at ~16.666ms.
        let tick = Duration::from_micros(16_666);

        while elapsed >= tick {
            if self.delay > 0 {
                self.delay -= 1;
            }
            if self.sound > 0 {
                self.sound -= 1;
            }
            self.last_timer_tick += tick;
            elapsed = now.duration_since(self.last_timer_tick);
        }
    }

    fn load_font(&mut self) {
        // Standard CHIP-8 4x5 font for 0..F (80 bytes)
        let font: [u8; 80] = [
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
            0xF0, 0x80, 0xF0, 0x80, 0x80, // F
        ];

        self.mem[FONT_BASE..FONT_BASE + font.len()].copy_from_slice(&font);
    }
}
