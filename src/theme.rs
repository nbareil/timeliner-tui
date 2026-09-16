use ratatui::style::{Color, Style};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Light,
    Dark,
}

impl Theme {
    pub fn toggle(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub fn palette(self) -> Palette {
        match self {
            Self::Light => Palette {
                canvas: rgb(0xf4f7fc),
                surface: rgb(0xffffff),
                alternate: rgb(0xf8faff),
                elevated: rgb(0xeaf0fa),
                border: rgb(0xc8d4e8),
                shadow: rgb(0xd9e1ef),
                text: rgb(0x25344d),
                muted: rgb(0x596b85),
                subtle: rgb(0x71839c),
                accent: rgb(0x315fce),
                selected: rgb(0xe3edff),
                teal: rgb(0x087d7a),
                teal_soft: rgb(0xdaf3ed),
                amber: rgb(0x92570c),
                amber_soft: rgb(0xffefd3),
                violet: rgb(0x7742bd),
                violet_soft: rgb(0xeee4ff),
                rose: rgb(0xb6335c),
                rose_soft: rgb(0xffe2ec),
            },
            Self::Dark => Palette {
                canvas: rgb(0x101725),
                surface: rgb(0x151e2e),
                alternate: rgb(0x182233),
                elevated: rgb(0x202d42),
                border: rgb(0x344763),
                shadow: rgb(0x090e18),
                text: rgb(0xe5edf9),
                muted: rgb(0xa6b6ce),
                subtle: rgb(0x8094b2),
                accent: rgb(0x96baff),
                selected: rgb(0x243c60),
                teal: rgb(0x72ddc8),
                teal_soft: rgb(0x163d3c),
                amber: rgb(0xf3c680),
                amber_soft: rgb(0x423424),
                violet: rgb(0xc6a5ff),
                violet_soft: rgb(0x352847),
                rose: rgb(0xff9db8),
                rose_soft: rgb(0x482839),
            },
        }
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: Color,
    pub surface: Color,
    pub alternate: Color,
    pub elevated: Color,
    pub border: Color,
    pub shadow: Color,
    pub text: Color,
    pub muted: Color,
    pub subtle: Color,
    pub accent: Color,
    pub selected: Color,
    pub teal: Color,
    pub teal_soft: Color,
    pub amber: Color,
    pub amber_soft: Color,
    pub violet: Color,
    pub violet_soft: Color,
    pub rose: Color,
    pub rose_soft: Color,
}

impl Palette {
    pub fn base(self) -> Style {
        Style::default().fg(self.text).bg(self.canvas)
    }

    pub fn host(self, hostname: &str) -> Color {
        let hash = hostname.bytes().fold(0_u64, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(byte as u64)
        });
        [self.teal, self.accent, self.violet, self.rose][hash as usize % 4]
    }
}

const fn rgb(value: u32) -> Color {
    Color::from_u32(value)
}

pub fn blend(from: Color, to: Color, step: u16, steps: u16) -> Color {
    let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (from, to) else {
        return from;
    };
    let steps = steps.max(1);
    let step = step.min(steps);
    let mix = |a: u8, b: u8| {
        ((a as u32 * (steps - step) as u32 + b as u32 * step as u32) / steps as u32) as u8
    };
    Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
}
