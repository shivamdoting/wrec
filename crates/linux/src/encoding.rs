use crate::{backend, pipeline::element};
use domain::{Codec, Quality, RecorderSettings, Result};
use gstreamer::{self as gst, prelude::*};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Va,
    Cuda,
    Nvidia,
    Software,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Mode {
    pub kind: Kind,
    pub factory: &'static str,
    pub dmabuf: bool,
}

pub(crate) fn modes(codec: Codec, pipewire: bool) -> Vec<Mode> {
    modes_with(codec, pipewire, |name| {
        gst::ElementFactory::find(name).is_some()
    })
}

fn modes_with(codec: Codec, pipewire: bool, has: impl Fn(&str) -> bool) -> Vec<Mode> {
    let (va, nv, software): (&[&str], &str, &[&str]) = match codec {
        Codec::H264 => (
            &["vah264lpenc", "vah264enc"],
            "nvh264enc",
            &["x264enc", "openh264enc"],
        ),
        Codec::Hevc => (&["vah265lpenc", "vah265enc"], "nvh265enc", &["x265enc"]),
    };
    let mut modes = Vec::new();
    for factory in va
        .iter()
        .copied()
        .filter(|name| has(name) && has("vapostproc"))
    {
        if pipewire {
            modes.push(Mode {
                kind: Kind::Va,
                factory,
                dmabuf: true,
            });
        }
        modes.push(Mode {
            kind: Kind::Va,
            factory,
            dmabuf: false,
        });
    }
    if has(nv) {
        if has("cudaupload") && has("cudaconvertscale") {
            modes.push(Mode {
                kind: Kind::Cuda,
                factory: nv,
                dmabuf: false,
            });
        }
        modes.push(Mode {
            kind: Kind::Nvidia,
            factory: nv,
            dmabuf: false,
        });
    }
    for factory in software.iter().copied().filter(|name| has(name)) {
        modes.push(Mode {
            kind: Kind::Software,
            factory,
            dmabuf: false,
        });
    }
    modes
}

impl Mode {
    pub fn description(self) -> String {
        let input = if self.dmabuf {
            "shared GPU buffers"
        } else {
            "system-memory capture"
        };
        let processing = match self.kind {
            Kind::Va | Kind::Cuda => "GPU conversion and encoding",
            Kind::Nvidia => "CPU conversion, NVIDIA hardware encoding",
            Kind::Software => "CPU conversion and software encoding",
        };
        format!("{input}; {processing}; {}", self.factory)
    }

    pub fn caps(self, size: Option<(i32, i32)>) -> gst::Caps {
        let builder =
            gst::Caps::builder("video/x-raw").field("pixel-aspect-ratio", gst::Fraction::new(1, 1));
        let builder = match self.kind {
            Kind::Va => builder
                .features(["memory:VAMemory"])
                .field("format", "NV12"),
            Kind::Cuda => builder
                .features(["memory:CUDAMemory"])
                .field("format", "NV12"),
            Kind::Nvidia => builder
                .features(["memory:SystemMemory"])
                .field("format", "NV12"),
            Kind::Software => builder
                .features(["memory:SystemMemory"])
                .field("format", "I420"),
        };
        match size {
            Some((w, h)) => builder.field("width", w).field("height", h).build(),
            None => builder.build(),
        }
    }

    pub fn converters(self) -> Result<Vec<gst::Element>> {
        match self.kind {
            Kind::Va => {
                let converter = element("vapostproc")?;
                converter.set_property("add-borders", true);
                Ok(vec![converter])
            }
            Kind::Cuda => {
                let converter = element("cudaconvertscale")?;
                if converter.find_property("add-borders").is_some() {
                    converter.set_property("add-borders", true);
                }
                Ok(vec![element("cudaupload")?, converter])
            }
            _ => {
                let scaler = element("videoscale")?;
                scaler.set_property("add-borders", true);
                Ok(vec![element("videoconvert")?, scaler])
            }
        }
    }

    pub fn encoder(self, settings: &RecorderSettings) -> Result<gst::Element> {
        let encoder = element(self.factory)?;
        let qp = match settings.quality {
            Quality::Efficient => 30u32,
            Quality::Balanced => 25u32,
            Quality::High => 20u32,
        };
        let bitrate = match settings.quality {
            Quality::Efficient => 2500u32,
            Quality::Balanced => 6000u32,
            Quality::High => 16000u32,
        };
        match self.kind {
            Kind::Va => {
                encoder.set_property_from_str("rate-control", "cqp");
                for name in ["qpi", "qpp", "qpb"] {
                    encoder.set_property(name, qp);
                }
                encoder.set_property("key-int-max", settings.fps.as_u32() * 2);
                encoder.set_property("b-frames", 0u32);
            }
            Kind::Cuda | Kind::Nvidia => {
                encoder.set_property("bitrate", bitrate);
                encoder.set_property("gop-size", settings.fps.as_u32() as i32 * 2);
                encoder.set_property("bframes", 0u32);
            }
            Kind::Software => match self.factory {
                "x264enc" => {
                    encoder.set_property_from_str("speed-preset", "ultrafast");
                    encoder.set_property_from_str("tune", "zerolatency");
                    encoder.set_property_from_str("pass", "qual");
                    encoder.set_property("quantizer", qp);
                    encoder.set_property("threads", 2u32);
                    encoder.set_property("key-int-max", settings.fps.as_u32() * 2);
                }
                "x265enc" => {
                    encoder.set_property_from_str("speed-preset", "ultrafast");
                    encoder.set_property_from_str("tune", "zerolatency");
                    encoder.set_property("qp", qp as i32);
                    encoder.set_property("option-string", "pools=2:frame-threads=2");
                    encoder.set_property("key-int-max", settings.fps.as_u32() as i32 * 2);
                }
                "openh264enc" => {
                    encoder.set_property("bitrate", bitrate * 1000);
                    encoder.set_property("multi-thread", 2u32);
                    encoder.set_property("gop-size", settings.fps.as_u32() * 2);
                }
                _ => return Err(backend("unsupported software encoder")),
            },
        }
        Ok(encoder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefers_hardware_and_retains_software_compatibility() {
        let modes = modes_with(Codec::H264, true, |_| true);
        assert!(modes[0].dmabuf);
        assert_eq!(modes[0].kind, Kind::Va);
        assert!(modes.iter().any(|m| m.kind == Kind::Cuda));
        assert_eq!(modes.last().unwrap().kind, Kind::Software);
        assert!(modes_with(Codec::Hevc, false, |_| true)
            .iter()
            .all(|m| !m.dmabuf));
        let modes = modes_with(Codec::H264, false, |name| name == "openh264enc");
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].kind, Kind::Software);
    }
}
