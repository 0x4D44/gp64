//! Integer-only YM3812 synthesis from the published register map.
//! Nine melodic channels render at 49,716 Hz.
//! Percussion mode (0xBD bit 5) is stored but not synthesised.

/// The chip's own output rate, its 3.579545 MHz master clock divided by 72.
pub const SAMPLE_RATE: u32 = 49_716;

/// Melodic channels on an OPL2.
pub const CHANNELS: usize = 9;

/// Two operators per channel.
pub const OPERATORS: usize = CHANNELS * 2;

/// Maximum attenuation, in the 1/256-octave units the ROMs use.
const SILENCE: u16 = 0xFFF;

/// The envelope's own range, nine bits of 0.1875 dB each.
const ENVELOPE_SILENCE: i16 = 0x1FF;

/// One full turn of the phase accumulator.
const PHASE_CYCLE: u32 = 1 << 20;

/// Entries in the log-sin quarter wave, so 1024 steps in a full turn.
const PHASE_STEPS: u32 = 1 << 10;

/// Envelope increment pattern, indexed by the low two bits of the rate and by
/// the phase of the global counter.  The four rows spread two, four, six and
/// eight increments over each group of eight steps, which is how the chip
/// interpolates between the coarse rates its counter can express directly.
#[rustfmt::skip]
const ENVELOPE_INCREMENT: [[u8; 8]; 4] = [
    [0, 1, 0, 1, 0, 1, 0, 1],
    [0, 1, 0, 1, 1, 1, 0, 1],
    [0, 1, 1, 1, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 1],
];

/// Right shift applied to the key-scale level for each of its four settings.
///
/// The settings are not in increasing order on this chip: `0` disables the
/// effect, and `1`, `2`, `3` select 3, 1.5 and 6 dB per octave.
const KEY_SCALE_SHIFT: [u8; 4] = [0, 1, 2, 0];

/// Where an operator's envelope currently is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    /// Silent, awaiting a key-on.
    Off,
    /// Rising from wherever the envelope stood toward full amplitude.
    Attack,
    /// Falling from full amplitude toward the sustain level.
    Decay,
    /// Post-decay while keyed; EG-TYPE decides whether this holds or falls.
    Sustain,
    /// Falling to silence after a key-off.
    Release,
}

/// One of the chip's eighteen operators.
#[derive(Clone, Copy, Debug)]
struct Operator {
    tremolo: bool,
    vibrato: bool,
    sustained: bool,
    key_scale_rate: bool,
    /// Twice the frequency multiplier, because half of the sixteen settings
    /// are halves.
    multiple: u8,
    key_scale_level: u8,
    total_level: u8,
    attack: u8,
    decay: u8,
    sustain: u8,
    release: u8,
    waveform: u8,
    phase: u32,
    envelope: i16,
    stage: Stage,
    output: i16,
    previous_output: i16,
}

impl Operator {
    const fn new() -> Self {
        Self {
            tremolo: false,
            vibrato: false,
            sustained: false,
            key_scale_rate: false,
            multiple: 1,
            key_scale_level: 0,
            total_level: 0,
            attack: 0,
            decay: 0,
            sustain: 0,
            release: 0,
            waveform: 0,
            phase: 0,
            envelope: ENVELOPE_SILENCE,
            stage: Stage::Off,
            output: 0,
            previous_output: 0,
        }
    }
}

/// One of the chip's nine melodic channels.
#[derive(Clone, Copy, Debug)]
struct Channel {
    frequency: u16,
    block: u8,
    keyed: bool,
    feedback: u8,
    /// True selects additive connection, false two-operator FM.
    additive: bool,
}

impl Channel {
    const fn new() -> Self {
        Self {
            frequency: 0,
            block: 0,
            keyed: false,
            feedback: 0,
            additive: false,
        }
    }
}

/// A complete OPL2.
///
#[derive(Clone, Copy, Debug)]
pub(super) struct Synth {
    operators: [Operator; OPERATORS],
    channels: [Channel; CHANNELS],
    /// Register `0x01` bit 5.  With it clear every operator is a plain sine
    /// whatever register `0xE0` says.
    waveform_select: bool,
    /// Register `0x08` bit 6, which picks the F-number bit that contributes
    /// to the key-scale number.
    note_select: bool,
    deep_tremolo: bool,
    deep_vibrato: bool,
    /// Register 0xBD bit 5; stored but not synthesised.
    percussion: bool,
    counter: u32,
}

impl Default for Synth {
    fn default() -> Self {
        Self::new()
    }
}

impl Synth {
    /// Creates a chip in its reset state, with every operator silent.
    pub(super) const fn new() -> Self {
        Self {
            operators: [Operator::new(); OPERATORS],
            channels: [Channel::new(); CHANNELS],
            waveform_select: false,
            note_select: false,
            deep_tremolo: false,
            deep_vibrato: false,
            percussion: false,
            counter: 0,
        }
    }

    /// Applies one register write, as though to ports `0x388` and `0x389`.
    ///
    /// Writes to registers the chip does not implement are discarded, which
    /// is what the hardware does; no input can panic.
    pub(super) fn write_register(&mut self, register: u8, value: u8) {
        match register {
            0x01 => self.waveform_select = value & 0x20 != 0,
            0x08 => self.note_select = value & 0x40 != 0,
            0x20..=0x35 => {
                if let Some(operator) = self.operator_mut(register - 0x20) {
                    operator.tremolo = value & 0x80 != 0;
                    operator.vibrato = value & 0x40 != 0;
                    operator.sustained = value & 0x20 != 0;
                    operator.key_scale_rate = value & 0x10 != 0;
                    operator.multiple = multiple_times_two(value & 0x0F);
                }
            }
            0x40..=0x55 => {
                if let Some(operator) = self.operator_mut(register - 0x40) {
                    operator.key_scale_level = value >> 6;
                    operator.total_level = value & 0x3F;
                }
            }
            0x60..=0x75 => {
                if let Some(operator) = self.operator_mut(register - 0x60) {
                    operator.attack = value >> 4;
                    operator.decay = value & 0x0F;
                }
            }
            0x80..=0x95 => {
                if let Some(operator) = self.operator_mut(register - 0x80) {
                    operator.sustain = value >> 4;
                    operator.release = value & 0x0F;
                }
            }
            0xA0..=0xA8 => {
                let index = usize::from(register - 0xA0);
                let channel = &mut self.channels[index];
                channel.frequency = (channel.frequency & 0x300) | u16::from(value);
            }
            0xB0..=0xB8 => {
                let index = usize::from(register - 0xB0);
                let channel = &mut self.channels[index];
                channel.frequency = (channel.frequency & 0x0FF) | (u16::from(value & 0x03) << 8);
                channel.block = (value >> 2) & 0x07;
                let keyed = value & 0x20 != 0;
                if keyed != channel.keyed {
                    channel.keyed = keyed;
                    self.set_key(index, keyed);
                }
            }
            0xBD => {
                self.deep_tremolo = value & 0x80 != 0;
                self.deep_vibrato = value & 0x40 != 0;
                self.percussion = value & 0x20 != 0;
            }
            0xC0..=0xC8 => {
                let channel = &mut self.channels[usize::from(register - 0xC0)];
                channel.feedback = (value >> 1) & 0x07;
                channel.additive = value & 0x01 != 0;
            }
            0xE0..=0xF5 => {
                if let Some(operator) = self.operator_mut(register - 0xE0) {
                    operator.waveform = value & 0x03;
                }
            }
            _ => {}
        }
    }

    /// Produces the next sample, summed across every channel and clipped the
    /// way the card's converter clips.
    pub(super) fn next_sample(&mut self) -> i16 {
        self.counter = self.counter.wrapping_add(1);

        let tremolo = self.tremolo_attenuation();
        let vibrato_phase = ((self.counter >> 10) & 0x07) as u8;

        let mut total = 0i32;
        for index in 0..CHANNELS {
            total += i32::from(self.advance_channel(index, tremolo, vibrato_phase));
        }
        total.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    }

    /// Fills `output` with consecutive samples.
    #[cfg(test)]
    fn generate(&mut self, output: &mut [i16]) {
        for sample in output.iter_mut() {
            *sample = self.next_sample();
        }
    }

    /// Reports whether every operator is silent, which lets a caller skip
    /// work when nothing is sounding.
    #[cfg(test)]
    fn is_silent(&self) -> bool {
        self.operators
            .iter()
            .all(|operator| operator.stage == Stage::Off)
    }

    /// Advances a run of silent chip samples without walking every channel.
    ///
    /// The caller must have established that every operator is off.
    /// An off operator still copies its previous output once before clearing
    /// it, so preserve that boundary when advancing exactly one sample.
    #[cfg(test)]
    fn advance_silence(&mut self, samples: u64) {
        if samples == 0 {
            return;
        }
        debug_assert!(self.is_silent());
        self.counter = self.counter.wrapping_add(samples as u32);
        if samples == 1 {
            for operator in &mut self.operators {
                operator.previous_output = operator.output;
                operator.output = 0;
            }
        } else {
            for operator in &mut self.operators {
                operator.previous_output = 0;
                operator.output = 0;
            }
        }
    }

    /// Resolves a register's operator offset to an operator index.
    ///
    /// The eighteen operators occupy a 0x16-wide offset space with two gaps,
    /// so a third of the offsets in each register block address nothing.
    fn operator_mut(&mut self, offset: u8) -> Option<&mut Operator> {
        let index = operator_index(offset)?;
        Some(&mut self.operators[index])
    }

    fn set_key(&mut self, channel: usize, keyed: bool) {
        for slot in 0..2 {
            let operator = &mut self.operators[channel * 2 + slot];
            if keyed {
                operator.stage = Stage::Attack;
                operator.phase = 0;
            } else if operator.stage != Stage::Off {
                operator.stage = Stage::Release;
            }
        }
    }

    /// The global tremolo, a triangle running at about 3.7 Hz.
    ///
    /// The result is in envelope units of 0.1875 dB, giving a depth of 4.8 dB
    /// when register `0xBD` asks for the deep setting and 1.2 dB when it does
    /// not.
    fn tremolo_attenuation(&self) -> i16 {
        let step = (self.counter >> 6) % 210;
        let triangle = if step < 105 { step } else { 209 - step } as i16;
        if self.deep_tremolo {
            triangle / 4
        } else {
            triangle / 16
        }
    }

    fn advance_channel(&mut self, index: usize, tremolo: i16, vibrato_phase: u8) -> i16 {
        let channel = self.channels[index];
        let modulator = index * 2;
        let carrier = modulator + 1;

        let feedback = if channel.feedback == 0 {
            0
        } else {
            let operator = &self.operators[modulator];
            (i32::from(operator.output) + i32::from(operator.previous_output))
                >> (9 - channel.feedback)
        };

        let modulator_output =
            self.advance_operator(modulator, &channel, feedback, tremolo, vibrato_phase);

        let carrier_modulation = if channel.additive {
            0
        } else {
            i32::from(modulator_output) >> 1
        };
        let carrier_output = self.advance_operator(
            carrier,
            &channel,
            carrier_modulation,
            tremolo,
            vibrato_phase,
        );

        if channel.additive {
            i32::from(modulator_output)
                .saturating_add(i32::from(carrier_output))
                .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
        } else {
            carrier_output
        }
    }

    fn advance_operator(
        &mut self,
        index: usize,
        channel: &Channel,
        modulation: i32,
        tremolo: i16,
        vibrato_phase: u8,
    ) -> i16 {
        self.advance_envelope(index, channel);

        let operator = self.operators[index];
        if operator.stage == Stage::Off {
            let operator = &mut self.operators[index];
            operator.previous_output = operator.output;
            operator.output = 0;
            return 0;
        }

        let frequency = if operator.vibrato {
            apply_vibrato(channel.frequency, vibrato_phase, self.deep_vibrato)
        } else {
            channel.frequency
        };

        let increment =
            ((u32::from(frequency) * u32::from(operator.multiple)) << channel.block) >> 1;
        let phase = (operator.phase + increment) % PHASE_CYCLE;

        let step =
            ((phase / (PHASE_CYCLE / PHASE_STEPS)) as i32 + modulation) as u32 & (PHASE_STEPS - 1);
        let (wave, negative) = self.waveform_sample(operator.waveform, step);

        let attenuation = wave
            .saturating_add(self.operator_attenuation(index, channel, tremolo))
            .min(SILENCE);
        let magnitude = attenuation_to_amplitude(attenuation);
        let sample = if negative { -magnitude } else { magnitude };

        let operator = &mut self.operators[index];
        operator.phase = phase;
        operator.previous_output = operator.output;
        operator.output = sample;
        sample
    }

    /// Everything attenuating an operator besides its own waveform: the
    /// envelope, the programmed level, the key-scale level and the tremolo,
    /// all converted into the ROMs' 1/256-octave units.
    fn operator_attenuation(&self, index: usize, channel: &Channel, tremolo: i16) -> u16 {
        let operator = &self.operators[index];
        let mut total = (operator.envelope.max(0) as u16) << 3;
        total = total.saturating_add(u16::from(operator.total_level) << 5);

        if operator.key_scale_level != 0 {
            let level = i16::from(KEY_SCALE_LEVEL[usize::from(channel.frequency >> 6)])
                - 8 * (7 - i16::from(channel.block));
            if level > 0 {
                let shift = KEY_SCALE_SHIFT[usize::from(operator.key_scale_level)];
                total = total.saturating_add(((level as u16) << 5) >> shift);
            }
        }

        if operator.tremolo {
            total = total.saturating_add((tremolo.max(0) as u16) << 3);
        }
        total.min(SILENCE)
    }

    /// Looks one step of a waveform up in the log-sin ROM.
    ///
    /// The ROM holds a quarter wave; bit 8 of the step mirrors it into the
    /// second quarter and bit 9 makes it negative.  The three derived OPL2
    /// waveforms silence or fold parts of that.
    fn waveform_sample(&self, waveform: u8, step: u32) -> (u16, bool) {
        let waveform = if self.waveform_select { waveform } else { 0 };
        let quarter = (step & 0xFF) as usize;
        let mirrored = if step & 0x100 != 0 {
            255 - quarter
        } else {
            quarter
        };
        let magnitude = LOG_SIN[mirrored];

        match waveform {
            0 => (magnitude, step & 0x200 != 0),
            1 => {
                if step & 0x200 != 0 {
                    (SILENCE, false)
                } else {
                    (magnitude, false)
                }
            }
            2 => (magnitude, false),
            _ => {
                if step & 0x100 != 0 {
                    (SILENCE, false)
                } else {
                    (LOG_SIN[quarter], false)
                }
            }
        }
    }

    fn advance_envelope(&mut self, index: usize, channel: &Channel) {
        let operator = self.operators[index];
        if operator.stage == Stage::Off {
            return;
        }
        if operator.stage == Stage::Sustain && operator.sustained {
            if operator.envelope >= ENVELOPE_SILENCE {
                self.check_stage_transition(index);
            }
            return;
        }

        let programmed = match operator.stage {
            Stage::Attack => operator.attack,
            Stage::Decay => operator.decay,
            Stage::Sustain | Stage::Release => operator.release,
            Stage::Off => 0,
        };

        if programmed == 0 {
            // A zero rate holds the envelope where it is, except that decay
            // still has to notice it reached its sustain target.
            self.check_stage_transition(index);
            return;
        }

        let key_scale = self.key_scale_number(channel);
        let shift = if operator.key_scale_rate { 0 } else { 2 };
        let rate = (4 * u16::from(programmed) + u16::from(key_scale >> shift)).min(63);
        let coarse = (rate >> 2) as u32;
        let fine = usize::from(rate & 0x03);

        let increment = if coarse < 13 {
            let period = 13 - coarse;
            if self.counter & ((1 << period) - 1) != 0 {
                self.check_stage_transition(index);
                return;
            }
            i16::from(ENVELOPE_INCREMENT[fine][((self.counter >> period) & 0x07) as usize])
        } else {
            let step = i16::from(ENVELOPE_INCREMENT[fine][(self.counter & 0x07) as usize]);
            step << (coarse - 13).min(3)
        };

        let operator = &mut self.operators[index];
        match operator.stage {
            Stage::Attack => {
                if increment > 0 {
                    // The attack curve is exponential in the attenuation
                    // domain: each step closes a fixed fraction of what is
                    // left, so it decelerates as it approaches full volume.
                    let level = i32::from(operator.envelope);
                    let step = ((!level) * i32::from(increment)) >> 3;
                    operator.envelope = (level + step).clamp(0, i32::from(ENVELOPE_SILENCE)) as i16;
                }
                if operator.envelope <= 0 {
                    operator.envelope = 0;
                    operator.stage = Stage::Decay;
                }
            }
            Stage::Decay | Stage::Sustain | Stage::Release => {
                operator.envelope = operator
                    .envelope
                    .saturating_add(increment)
                    .min(ENVELOPE_SILENCE);
            }
            Stage::Off => {}
        }
        self.check_stage_transition(index);
    }

    fn check_stage_transition(&mut self, index: usize) {
        let operator = &mut self.operators[index];
        match operator.stage {
            Stage::Decay => {
                if operator.envelope >= sustain_attenuation(operator.sustain) {
                    // EG-TYPE remains live after decay, so keyed percussive
                    // notes also stay here and use RR until key-off.
                    operator.stage = Stage::Sustain;
                }
            }
            Stage::Sustain | Stage::Release => {
                if operator.envelope >= ENVELOPE_SILENCE {
                    operator.envelope = ENVELOPE_SILENCE;
                    operator.stage = Stage::Off;
                }
            }
            Stage::Attack | Stage::Off => {}
        }
    }

    /// The key-scale number, which is what makes envelopes run faster for
    /// higher notes.
    fn key_scale_number(&self, channel: &Channel) -> u8 {
        let bit = if self.note_select {
            (channel.frequency >> 8) & 1
        } else {
            (channel.frequency >> 9) & 1
        };
        (channel.block << 1) | bit as u8
    }
}

/// Twice the frequency multiplier for each of the sixteen settings.
///
/// Setting 0 is a half, and settings 11, 13 and 15 repeat their neighbours,
/// which is why the table is not simply the index.
const fn multiple_times_two(setting: u8) -> u8 {
    match setting {
        0 => 1,
        11 => 20,
        13 => 24,
        14 | 15 => 30,
        other => other * 2,
    }
}

/// Resolves a register offset within a block to an operator index.
const fn operator_index(offset: u8) -> Option<usize> {
    let group = offset / 8;
    let slot = offset % 8;
    if group > 2 || slot > 5 {
        return None;
    }
    // Each group of eight offsets holds three modulators then three carriers,
    // for three consecutive channels.
    let channel = group * 3 + slot % 3;
    let is_carrier = slot / 3;
    Some((channel * 2 + is_carrier) as usize)
}

/// The attenuation at which a decay hands over to the sustain stage.
///
/// Each of the sixteen settings is 3 dB, except that the last means silence
/// rather than the 45 dB the pattern would give.
const fn sustain_attenuation(setting: u8) -> i16 {
    if setting == 0x0F {
        ENVELOPE_SILENCE
    } else {
        (setting as i16) * 16
    }
}

/// Bends an F-number by the global vibrato.
///
/// The depth is a fraction of the note's own F-number rather than a fixed
/// interval, which is what keeps the effect a constant number of cents across
/// the range.
fn apply_vibrato(frequency: u16, phase: u8, deep: bool) -> u16 {
    let mut depth = frequency >> 7;
    if !deep {
        depth >>= 1;
    }
    let offset = match phase & 0x07 {
        0 | 4 => 0i32,
        1 | 3 => i32::from(depth >> 1),
        2 => i32::from(depth),
        5 | 7 => -i32::from(depth >> 1),
        _ => -i32::from(depth),
    };
    (i32::from(frequency) + offset).clamp(0, 0x3FF) as u16
}

/// Converts a total attenuation into a linear amplitude.
///
/// The low eight bits index one octave of the exponent ROM and the high bits
/// say how many octaves to shift it down, which is what makes the conversion
/// a lookup and a shift rather than a power.
fn attenuation_to_amplitude(attenuation: u16) -> i16 {
    let attenuation = attenuation.min(SILENCE);
    let mantissa = i32::from(EXPONENT[usize::from(attenuation & 0xFF)]);
    let octaves = u32::from(attenuation >> 8);
    ((mantissa << 1) >> octaves.min(15)) as i16
}

// Generated by tools/gen_opl2_tables.rs; do not edit by hand.

#[rustfmt::skip]
static LOG_SIN: [u16; 256] = [
    0x859, 0x6c3, 0x607, 0x58b, 0x52e, 0x4e4, 0x4a6, 0x471,
    0x443, 0x41a, 0x3f5, 0x3d3, 0x3b5, 0x398, 0x37e, 0x365,
    0x34e, 0x339, 0x324, 0x311, 0x2ff, 0x2ed, 0x2dc, 0x2cd,
    0x2bd, 0x2af, 0x2a0, 0x293, 0x286, 0x279, 0x26d, 0x261,
    0x256, 0x24b, 0x240, 0x236, 0x22c, 0x222, 0x218, 0x20f,
    0x206, 0x1fd, 0x1f5, 0x1ec, 0x1e4, 0x1dc, 0x1d4, 0x1cd,
    0x1c5, 0x1be, 0x1b7, 0x1b0, 0x1a9, 0x1a2, 0x19b, 0x195,
    0x18f, 0x188, 0x182, 0x17c, 0x177, 0x171, 0x16b, 0x166,
    0x160, 0x15b, 0x155, 0x150, 0x14b, 0x146, 0x141, 0x13c,
    0x137, 0x133, 0x12e, 0x129, 0x125, 0x121, 0x11c, 0x118,
    0x114, 0x10f, 0x10b, 0x107, 0x103, 0x0ff, 0x0fb, 0x0f8,
    0x0f4, 0x0f0, 0x0ec, 0x0e9, 0x0e5, 0x0e2, 0x0de, 0x0db,
    0x0d7, 0x0d4, 0x0d1, 0x0cd, 0x0ca, 0x0c7, 0x0c4, 0x0c1,
    0x0be, 0x0bb, 0x0b8, 0x0b5, 0x0b2, 0x0af, 0x0ac, 0x0a9,
    0x0a7, 0x0a4, 0x0a1, 0x09f, 0x09c, 0x099, 0x097, 0x094,
    0x092, 0x08f, 0x08d, 0x08a, 0x088, 0x086, 0x083, 0x081,
    0x07f, 0x07d, 0x07a, 0x078, 0x076, 0x074, 0x072, 0x070,
    0x06e, 0x06c, 0x06a, 0x068, 0x066, 0x064, 0x062, 0x060,
    0x05e, 0x05c, 0x05b, 0x059, 0x057, 0x055, 0x053, 0x052,
    0x050, 0x04e, 0x04d, 0x04b, 0x04a, 0x048, 0x046, 0x045,
    0x043, 0x042, 0x040, 0x03f, 0x03e, 0x03c, 0x03b, 0x039,
    0x038, 0x037, 0x035, 0x034, 0x033, 0x031, 0x030, 0x02f,
    0x02e, 0x02d, 0x02b, 0x02a, 0x029, 0x028, 0x027, 0x026,
    0x025, 0x024, 0x023, 0x022, 0x021, 0x020, 0x01f, 0x01e,
    0x01d, 0x01c, 0x01b, 0x01a, 0x019, 0x018, 0x017, 0x017,
    0x016, 0x015, 0x014, 0x014, 0x013, 0x012, 0x011, 0x011,
    0x010, 0x00f, 0x00f, 0x00e, 0x00d, 0x00d, 0x00c, 0x00c,
    0x00b, 0x00a, 0x00a, 0x009, 0x009, 0x008, 0x008, 0x007,
    0x007, 0x007, 0x006, 0x006, 0x005, 0x005, 0x005, 0x004,
    0x004, 0x004, 0x003, 0x003, 0x003, 0x002, 0x002, 0x002,
    0x002, 0x001, 0x001, 0x001, 0x001, 0x001, 0x001, 0x001,
    0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000,
];

#[rustfmt::skip]
static EXPONENT: [u16; 256] = [
    0x7fa, 0x7f5, 0x7ef, 0x7ea, 0x7e4, 0x7df, 0x7da, 0x7d4,
    0x7cf, 0x7c9, 0x7c4, 0x7bf, 0x7b9, 0x7b4, 0x7ae, 0x7a9,
    0x7a4, 0x79f, 0x799, 0x794, 0x78f, 0x78a, 0x784, 0x77f,
    0x77a, 0x775, 0x770, 0x76a, 0x765, 0x760, 0x75b, 0x756,
    0x751, 0x74c, 0x747, 0x742, 0x73d, 0x738, 0x733, 0x72e,
    0x729, 0x724, 0x71f, 0x71a, 0x715, 0x710, 0x70b, 0x706,
    0x702, 0x6fd, 0x6f8, 0x6f3, 0x6ee, 0x6e9, 0x6e5, 0x6e0,
    0x6db, 0x6d6, 0x6d2, 0x6cd, 0x6c8, 0x6c4, 0x6bf, 0x6ba,
    0x6b5, 0x6b1, 0x6ac, 0x6a8, 0x6a3, 0x69e, 0x69a, 0x695,
    0x691, 0x68c, 0x688, 0x683, 0x67f, 0x67a, 0x676, 0x671,
    0x66d, 0x668, 0x664, 0x65f, 0x65b, 0x657, 0x652, 0x64e,
    0x649, 0x645, 0x641, 0x63c, 0x638, 0x634, 0x630, 0x62b,
    0x627, 0x623, 0x61e, 0x61a, 0x616, 0x612, 0x60e, 0x609,
    0x605, 0x601, 0x5fd, 0x5f9, 0x5f5, 0x5f0, 0x5ec, 0x5e8,
    0x5e4, 0x5e0, 0x5dc, 0x5d8, 0x5d4, 0x5d0, 0x5cc, 0x5c8,
    0x5c4, 0x5c0, 0x5bc, 0x5b8, 0x5b4, 0x5b0, 0x5ac, 0x5a8,
    0x5a4, 0x5a0, 0x59c, 0x599, 0x595, 0x591, 0x58d, 0x589,
    0x585, 0x581, 0x57e, 0x57a, 0x576, 0x572, 0x56f, 0x56b,
    0x567, 0x563, 0x560, 0x55c, 0x558, 0x554, 0x551, 0x54d,
    0x549, 0x546, 0x542, 0x53e, 0x53b, 0x537, 0x534, 0x530,
    0x52c, 0x529, 0x525, 0x522, 0x51e, 0x51b, 0x517, 0x514,
    0x510, 0x50c, 0x509, 0x506, 0x502, 0x4ff, 0x4fb, 0x4f8,
    0x4f4, 0x4f1, 0x4ed, 0x4ea, 0x4e7, 0x4e3, 0x4e0, 0x4dc,
    0x4d9, 0x4d6, 0x4d2, 0x4cf, 0x4cc, 0x4c8, 0x4c5, 0x4c2,
    0x4be, 0x4bb, 0x4b8, 0x4b5, 0x4b1, 0x4ae, 0x4ab, 0x4a8,
    0x4a4, 0x4a1, 0x49e, 0x49b, 0x498, 0x494, 0x491, 0x48e,
    0x48b, 0x488, 0x485, 0x482, 0x47e, 0x47b, 0x478, 0x475,
    0x472, 0x46f, 0x46c, 0x469, 0x466, 0x463, 0x460, 0x45d,
    0x45a, 0x457, 0x454, 0x451, 0x44e, 0x44b, 0x448, 0x445,
    0x442, 0x43f, 0x43c, 0x439, 0x436, 0x433, 0x430, 0x42d,
    0x42a, 0x428, 0x425, 0x422, 0x41f, 0x41c, 0x419, 0x416,
    0x414, 0x411, 0x40e, 0x40b, 0x408, 0x406, 0x403, 0x400,
];

#[rustfmt::skip]
static KEY_SCALE_LEVEL: [u8; 16] = [
    0, 32, 40, 45, 48, 51, 53, 55, 56, 58, 59, 60, 61, 62, 63, 64,
];

#[cfg(test)]
mod tests {
    use super::{
        attenuation_to_amplitude, operator_index, Stage, Synth, CHANNELS, EXPONENT,
        KEY_SCALE_LEVEL, LOG_SIN, OPERATORS, SAMPLE_RATE, SILENCE,
    };
    use std::f64::consts::PI;
    use std::vec::Vec;

    /// Programs channel 0 as a plain sustained sine at the given pitch.
    ///
    /// The modulator is attenuated to silence and feedback is off, so the
    /// carrier's waveform reaches the output unmodulated.
    fn sine_channel(frequency: u16, block: u8, level: u8) -> Synth {
        let mut chip = Synth::new();
        chip.write_register(0x01, 0x20);
        chip.write_register(0x20, 0x21);
        chip.write_register(0x23, 0x21);
        chip.write_register(0x40, 0x3F);
        chip.write_register(0x43, level);
        chip.write_register(0x60, 0xF0);
        chip.write_register(0x63, 0xF0);
        chip.write_register(0x80, 0x00);
        chip.write_register(0x83, 0x00);
        chip.write_register(0xC0, 0x00);
        chip.write_register(0xA0, (frequency & 0xFF) as u8);
        chip.write_register(0xB0, 0x20 | (block << 2) | ((frequency >> 8) & 0x03) as u8);
        chip
    }

    fn settle(chip: &mut Synth, samples: usize) {
        for _ in 0..samples {
            chip.next_sample();
        }
    }

    fn collect(chip: &mut Synth, samples: usize) -> Vec<i16> {
        (0..samples).map(|_| chip.next_sample()).collect()
    }

    fn rising_zero_crossings(samples: &[i16]) -> usize {
        samples
            .windows(2)
            .filter(|pair| pair[0] < 0 && pair[1] >= 0)
            .count()
    }

    fn peak(samples: &[i16]) -> i32 {
        samples
            .iter()
            .map(|sample| i32::from(*sample).abs())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn log_sin_rom_is_its_closed_form() {
        for (index, stored) in LOG_SIN.iter().enumerate() {
            let angle = (index as f64 + 0.5) * PI / 512.0;
            let expected = (-angle.sin().log2() * 256.0).round() as u16;
            assert_eq!(*stored, expected, "log-sin entry {index}");
        }
    }

    #[test]
    fn exponent_rom_is_its_closed_form() {
        for (index, stored) in EXPONENT.iter().enumerate() {
            let expected = (2.0_f64.powf((255 - index) as f64 / 256.0) * 1024.0).round() as u16;
            assert_eq!(*stored, expected, "exponent entry {index}");
        }
    }

    #[test]
    fn key_scale_level_rom_is_its_closed_form() {
        assert_eq!(KEY_SCALE_LEVEL[0], 0);
        for (index, stored) in KEY_SCALE_LEVEL.iter().enumerate().skip(1) {
            let expected = ((8.0 * (index as f64).log2()).ceil() as u8) + 32;
            assert_eq!(*stored, expected, "key-scale entry {index}");
        }
    }

    #[test]
    fn operator_offsets_cover_every_operator_exactly_once() {
        let mut seen = [false; OPERATORS];
        let mut mapped = 0;
        for offset in 0..=0x15u8 {
            match operator_index(offset) {
                Some(index) => {
                    assert!(
                        !seen[index],
                        "offset {offset:#04x} repeats operator {index}"
                    );
                    seen[index] = true;
                    mapped += 1;
                }
                None => assert!(
                    offset % 8 > 5,
                    "offset {offset:#04x} should have mapped to an operator"
                ),
            }
        }
        assert_eq!(mapped, OPERATORS);
        assert!(seen.iter().all(|hit| *hit));
    }

    #[test]
    fn documented_operator_offsets_resolve_to_their_channels() {
        // YM3812 operator offsets for the nine melodic channels.
        let modulators = [0x00u8, 0x01, 0x02, 0x08, 0x09, 0x0A, 0x10, 0x11, 0x12];
        let carriers = [0x03u8, 0x04, 0x05, 0x0B, 0x0C, 0x0D, 0x13, 0x14, 0x15];
        for channel in 0..CHANNELS {
            assert_eq!(operator_index(modulators[channel]), Some(channel * 2));
            assert_eq!(operator_index(carriers[channel]), Some(channel * 2 + 1));
        }
    }

    #[test]
    fn every_register_write_is_accepted_without_panicking() {
        let mut chip = Synth::new();
        for register in 0..=u8::MAX {
            for value in 0..=u8::MAX {
                chip.write_register(register, value);
            }
        }
        settle(&mut chip, 512);
    }

    #[test]
    fn a_reset_chip_is_silent() {
        let mut chip = Synth::new();
        assert!(chip.is_silent());
        assert!(collect(&mut chip, 1_024).iter().all(|sample| *sample == 0));
    }

    #[test]
    fn bulk_silence_matches_repeated_off_samples_and_clears_outputs() {
        for samples in [0, 1, 2, 9] {
            let mut bulk = Synth::new();
            bulk.counter = u32::MAX - 3;
            bulk.operators[0].output = 123;
            bulk.operators[0].previous_output = -456;
            let mut repeated = bulk;

            for _ in 0..samples {
                repeated.next_sample();
            }
            bulk.advance_silence(samples);

            assert_eq!(bulk.counter, repeated.counter);
            for (bulk, repeated) in bulk.operators.iter().zip(repeated.operators) {
                assert_eq!(bulk.stage, repeated.stage);
                assert_eq!(bulk.envelope, repeated.envelope);
                assert_eq!(bulk.phase, repeated.phase);
                assert_eq!(bulk.output, repeated.output);
                assert_eq!(bulk.previous_output, repeated.previous_output);
            }
        }
    }

    #[test]
    fn tone_frequency_follows_the_f_number_and_block() {
        // The published relation is f = fnum * 49716 / 2^(20 - block), and one
        // second of samples gives that count of rising zero crossings.
        for (frequency, block) in [(0x200u16, 4u8), (0x100, 5), (0x2A0, 3)] {
            let mut chip = sine_channel(frequency, block, 0x00);
            settle(&mut chip, 4_096);
            let samples = collect(&mut chip, SAMPLE_RATE as usize);
            let measured = rising_zero_crossings(&samples) as f64;
            let expected =
                f64::from(frequency) * f64::from(SAMPLE_RATE) / f64::from(1u32 << (20 - block));
            assert!(
                (measured - expected).abs() < expected * 0.01,
                "fnum {frequency:#x} block {block}: measured {measured} Hz, expected {expected}"
            );
        }
    }

    #[test]
    fn eight_total_level_steps_halve_the_amplitude() {
        // Each step is 0.75 dB, so eight of them are the 6 dB that halves it.
        let mut loud = sine_channel(0x200, 4, 0x00);
        let mut quiet = sine_channel(0x200, 4, 0x08);
        settle(&mut loud, 4_096);
        settle(&mut quiet, 4_096);
        let loud_peak = peak(&collect(&mut loud, 4_096));
        let quiet_peak = peak(&collect(&mut quiet, 4_096));

        assert!(loud_peak > 0 && quiet_peak > 0);
        let ratio = f64::from(loud_peak) / f64::from(quiet_peak);
        assert!(
            (ratio - 2.0).abs() < 0.05,
            "expected a halving, got {loud_peak} against {quiet_peak} (ratio {ratio})"
        );
    }

    #[test]
    fn a_sustaining_operator_holds_until_the_key_is_released() {
        let mut chip = sine_channel(0x200, 4, 0x00);
        // A real release rate on both operators, so that a sustain which
        // wrongly kept decaying would actually be visible here.
        chip.write_register(0x80, 0x0C);
        chip.write_register(0x83, 0x0C);
        settle(&mut chip, 4_096);
        let held = peak(&collect(&mut chip, 4_096));

        settle(&mut chip, SAMPLE_RATE as usize);
        let still_held = peak(&collect(&mut chip, 4_096));
        assert_eq!(
            held, still_held,
            "a sustained note must not fade while the key is down"
        );

        // Release it and it must reach true silence, not merely get quieter.
        chip.write_register(0xB0, 0x00);
        settle(&mut chip, SAMPLE_RATE as usize);
        assert!(chip.is_silent(), "a released note must reach the off stage");
        assert!(collect(&mut chip, 1_024).iter().all(|sample| *sample == 0));
    }

    #[test]
    fn clearing_eg_type_fades_a_keyed_post_decay_operator() {
        let mut chip = Synth::new();
        chip.write_register(0x23, 0x21);
        chip.write_register(0x83, 0x0F);
        chip.channels[0].keyed = true;
        let operator = &mut chip.operators[1];
        operator.stage = Stage::Sustain;
        operator.envelope = 300;
        operator.phase = 0x12345;

        chip.write_register(0x23, 0x01);

        assert_eq!(chip.operators[1].stage, Stage::Sustain);
        assert_eq!(chip.operators[1].envelope, 300);
        assert_eq!(chip.operators[1].phase, 0x12345);
        assert!(chip.channels[0].keyed);
        assert!(!chip.operators[1].sustained);

        let channel = chip.channels[0];
        for counter in 0..32 {
            chip.counter = counter;
            chip.advance_envelope(1, &channel);
        }

        assert!(
            chip.operators[1].envelope > 300,
            "clearing EG-TYPE while KON stays set must fade at RR"
        );
        assert_eq!(chip.operators[1].stage, Stage::Sustain);
    }

    #[test]
    fn percussive_post_decay_can_be_switched_to_a_hold() {
        let mut chip = Synth::new();
        chip.write_register(0x23, 0x01);
        chip.write_register(0x83, 0x0F);
        chip.channels[0].keyed = true;
        let operator = &mut chip.operators[1];
        operator.stage = Stage::Decay;
        operator.envelope = 0;
        operator.phase = 0x23456;

        let channel = chip.channels[0];
        chip.advance_envelope(1, &channel);

        assert_eq!(
            chip.operators[1].stage,
            Stage::Sustain,
            "keyed post-decay must remain distinct from key-off release"
        );
        let envelope = chip.operators[1].envelope;
        chip.write_register(0x23, 0x21);
        assert_eq!(chip.operators[1].stage, Stage::Sustain);
        assert_eq!(chip.operators[1].envelope, envelope);
        assert_eq!(chip.operators[1].phase, 0x23456);
        assert!(chip.channels[0].keyed);

        let channel = chip.channels[0];
        for counter in 0..32 {
            chip.counter = counter;
            chip.advance_envelope(1, &channel);
        }

        assert_eq!(chip.operators[1].envelope, envelope);
        assert_eq!(chip.operators[1].stage, Stage::Sustain);
    }

    #[test]
    fn key_off_releases_after_eg_type_changes_in_both_modes() {
        for (initial, changed) in [(0x21, 0x01), (0x01, 0x21)] {
            let mut chip = Synth::new();
            chip.write_register(0x23, initial);
            chip.write_register(0x83, 0x0F);
            chip.channels[0].keyed = true;
            chip.operators[1].stage = Stage::Sustain;
            chip.operators[1].envelope = 100;
            chip.write_register(0xB0, 0x00);
            assert_eq!(chip.operators[1].stage, Stage::Release);

            chip.write_register(0x23, changed);
            let channel = chip.channels[0];
            for counter in 0..32 {
                chip.counter = counter;
                chip.advance_envelope(1, &channel);
            }

            assert!(
                chip.operators[1].envelope > 100,
                "key-off must keep releasing after an EG-TYPE write"
            );
            assert_eq!(chip.operators[1].stage, Stage::Release);
        }
    }

    #[test]
    fn percussive_post_decay_with_zero_release_rate_holds() {
        let mut chip = Synth::new();
        chip.write_register(0x23, 0x01);
        chip.write_register(0x63, 0x00);
        chip.write_register(0x83, 0x00);
        chip.channels[0].keyed = true;
        let channel = chip.channels[0];
        chip.operators[1].stage = Stage::Decay;
        chip.operators[1].envelope = 0;
        chip.advance_envelope(1, &channel);

        assert_eq!(chip.operators[1].stage, Stage::Sustain);

        for counter in 0..64 {
            chip.counter = counter;
            chip.advance_envelope(1, &channel);
        }

        assert_eq!(chip.operators[1].envelope, 0);
        assert_eq!(chip.operators[1].stage, Stage::Sustain);
    }

    #[test]
    fn a_percussive_operator_decays_to_silence_with_the_key_still_down() {
        let mut chip = sine_channel(0x200, 4, 0x00);
        // Clear the sustaining bit on the carrier and give it a real decay.
        chip.write_register(0x23, 0x01);
        chip.write_register(0x63, 0xF8);
        chip.write_register(0x83, 0x08);
        chip.write_register(0xB0, 0x00);
        chip.write_register(0xB0, 0x20 | (4 << 2) | 0x02);
        settle(&mut chip, 1_024);
        let early = peak(&collect(&mut chip, 2_048));
        settle(&mut chip, SAMPLE_RATE as usize / 2);
        let late = peak(&collect(&mut chip, 2_048));
        assert!(
            late < early,
            "a percussive note must fade with the key down: {early} then {late}"
        );
    }

    #[test]
    fn key_on_restarts_the_phase_and_the_envelope() {
        let mut chip = sine_channel(0x200, 4, 0x00);
        settle(&mut chip, 4_096);
        // Key off without disturbing the block or F-number, so the phase goes
        // on advancing; clearing them as well would stop it and leave a zero
        // that says nothing about key-on.
        chip.write_register(0xB0, (4 << 2) | 0x02);
        // Deliberately not a whole number of cycles: at this pitch the phase
        // returns to zero every 128 samples, so a multiple of that would pass
        // whether or not key-on reset anything.
        settle(&mut chip, 100);
        assert!(
            chip.operators[0].phase != 0 && chip.operators[1].phase != 0,
            "the fixture must leave a phase for key-on to reset"
        );

        chip.write_register(0xB0, 0x20 | (4 << 2) | 0x02);
        assert!(
            chip.operators[0].phase == 0 && chip.operators[1].phase == 0,
            "key-on must reset both operators' phase"
        );
        assert_eq!(chip.operators[1].stage, Stage::Attack);
    }

    #[test]
    fn waveform_select_gates_the_three_derived_waveforms() {
        // With register 0x01 bit 5 clear the chip is a plain sine whatever
        // register 0xE0 asks for, so the half-sine must be indistinguishable.
        let mut gated = sine_channel(0x200, 4, 0x00);
        gated.write_register(0x01, 0x00);
        gated.write_register(0xE3, 0x01);
        let mut plain = sine_channel(0x200, 4, 0x00);
        plain.write_register(0x01, 0x00);
        settle(&mut gated, 512);
        settle(&mut plain, 512);
        assert_eq!(collect(&mut gated, 2_048), collect(&mut plain, 2_048));

        // Enabled, the half-sine has to differ, and it must never go negative.
        let mut enabled = sine_channel(0x200, 4, 0x00);
        enabled.write_register(0xE3, 0x01);
        settle(&mut enabled, 512);
        let half = collect(&mut enabled, 2_048);
        let mut reference = sine_channel(0x200, 4, 0x00);
        settle(&mut reference, 512);
        assert_ne!(half, collect(&mut reference, 2_048));
        assert!(half.iter().all(|sample| *sample >= 0));

        // Non-negative alone would also describe the absolute sine, so pin
        // the half-sine down by its silent half: at this pitch one cycle is
        // 128 samples, and half of each must be exactly zero.
        let silent = half.iter().filter(|sample| **sample == 0).count();
        assert!(
            (silent as i32 - 1_024).abs() < 64,
            "a half-sine must be silent for half its cycle, got {silent} of 2048"
        );
    }

    #[test]
    fn attenuation_conversion_halves_every_octave() {
        // The exponent ROM tops out at 0x7FA, so full scale is twice that.
        let full = attenuation_to_amplitude(0);
        assert_eq!(full, 4_084);
        for octave in 1..12u16 {
            let expected = full >> octave;
            let measured = attenuation_to_amplitude(octave << 8);
            assert!(
                (i32::from(measured) - i32::from(expected)).abs() <= 1,
                "octave {octave}: {measured} against {expected}"
            );
        }
        assert_eq!(attenuation_to_amplitude(SILENCE), 0);
    }

    #[test]
    fn a_rate_of_zero_holds_the_envelope_where_it_stands() {
        // Rate zero is not "instant" and not "slowest": the envelope does not
        // move at all, which is why a note with a zero release rate never
        // reaches silence.
        let mut chip = sine_channel(0x200, 4, 0x00);
        chip.write_register(0x80, 0x00);
        chip.write_register(0x83, 0x00);
        settle(&mut chip, 4_096);
        chip.write_register(0xB0, 0x00);
        settle(&mut chip, SAMPLE_RATE as usize);

        assert_eq!(chip.operators[1].stage, Stage::Release);
        assert_eq!(
            chip.operators[1].envelope, 0,
            "a zero release rate must leave the envelope untouched"
        );
        assert!(!chip.is_silent());
    }

    #[test]
    fn identical_register_streams_produce_identical_samples() {
        let writes = [
            (0x01u8, 0x20u8),
            (0x20, 0x31),
            (0x23, 0x21),
            (0x40, 0x10),
            (0x43, 0x00),
            (0x60, 0xF2),
            (0x63, 0xF1),
            (0x80, 0x53),
            (0x83, 0x44),
            (0xC0, 0x0D),
            (0xE0, 0x02),
            (0xE3, 0x01),
            (0xA0, 0x81),
            (0xB0, 0x31),
        ];
        let mut first = Synth::new();
        let mut second = Synth::new();
        for (register, value) in writes {
            first.write_register(register, value);
            second.write_register(register, value);
        }
        assert_eq!(collect(&mut first, 8_192), collect(&mut second, 8_192));
    }

    #[test]
    fn generate_matches_repeated_next_sample() {
        let mut streamed = sine_channel(0x180, 4, 0x04);
        let mut buffered = sine_channel(0x180, 4, 0x04);
        let expected = collect(&mut streamed, 1_000);
        let mut actual = [0i16; 1_000];
        buffered.generate(&mut actual);
        assert_eq!(&actual[..], &expected[..]);
    }
}
