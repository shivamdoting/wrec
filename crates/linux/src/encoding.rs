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

pub(crate) fn modes(codec: Codec, dmabuf: bool) -> Vec<Mode> {
    modes_with(codec, dmabuf, |name| {
        gst::ElementFactory::find(name).is_some()
    })
}

pub(crate) fn plugin_version(factory: &gst::ElementFactory) -> Option<Vec<u32>> {
    factory.plugin().map(|plugin| {
        plugin
            .version()
            .split('.')
            .map_while(|part| part.parse().ok())
            .collect()
    })
}

fn imports_dma_buf(sink: &gst::CapsRef, version: &[u32]) -> bool {
    let dma_drm = gst::Structure::builder("video/x-raw")
        .field("format", "DMA_DRM")
        .build();
    version >= [1, 24, 6].as_slice()
        && sink.iter_with_features().any(|(structure, features)| {
            !features.is_any()
                && features.contains("memory:DMABuf")
                && structure.has_field("format")
                && structure.can_intersect(&dma_drm)
        })
}

pub(crate) fn va_imports_dma_buf() -> bool {
    gst::ElementFactory::find("vapostproc").is_some_and(|factory| {
        let version = plugin_version(&factory).unwrap_or_default();
        factory.static_pad_templates().iter().any(|template| {
            template.direction() == gst::PadDirection::Sink
                && imports_dma_buf(&template.caps(), &version)
        })
    })
}

fn modes_with(codec: Codec, dmabuf: bool, has: impl Fn(&str) -> bool) -> Vec<Mode> {
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
        if dmabuf {
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
                    // A second frame thread keeps each frame until the next one
                    // arrives, a second later on an idle screen. A third worker
                    // maintains throughput without keeping a second frame.
                    encoder.set_property("option-string", "pools=3:frame-threads=1");
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
    fn shared_gpu_buffers_need_explicit_dma_drm_and_a_sizeless_importer() {
        gst::init().unwrap();
        let caps = |description: &str| description.parse::<gst::Caps>().unwrap();
        let native = caps("video/x-raw(memory:VAMemory), format={ NV12, P010_10LE }; video/x-raw(memory:DMABuf), format={ BGRA, RGBA, BGRx, RGBx, NV12, P010_10LE }; video/x-raw(ANY); video/x-raw(ANY), format={ BGRx, NV12 }");
        let formatless = caps("video/x-raw(memory:DMABuf), width=[1, 16384]");
        let modern = caps("video/x-raw(memory:VAMemory), format={ NV12, P010_10LE }; video/x-raw(memory:DMABuf), format=DMA_DRM, drm-format={ NV12:0x0200000000000901, XR24:0x0200000000000901 }; video/x-raw, format={ BGRx, NV12 }");
        assert!(!imports_dma_buf(&native, &[1, 22, 0]));
        assert!(!imports_dma_buf(&native, &[1, 28, 2]));
        assert!(!imports_dma_buf(&formatless, &[1, 28, 2]));
        assert!(!imports_dma_buf(&modern, &[1, 24, 0]));
        assert!(!imports_dma_buf(&modern, &[1, 24, 5]));
        assert!(imports_dma_buf(&modern, &[1, 24, 6]));
        assert!(imports_dma_buf(&modern, &[1, 28, 2]));
        if let Some(factory) = gst::ElementFactory::find("vapostproc") {
            if plugin_version(&factory).is_some_and(|version| version >= vec![1, 24, 6]) {
                assert!(va_imports_dma_buf());
            }
        }
    }

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

    // An idle screen sends about one frame a second. The movie muxer holds
    // audio until video reaches it, so an encoder that keeps a frame until the
    // next one arrives backs audio up for seconds.
    #[test]
    fn software_encoders_emit_each_frame_before_the_next_arrives() {
        gst::init().unwrap();
        let mut tested = Vec::new();
        for codec in [Codec::H264, Codec::Hevc] {
            for mode in modes(codec, false) {
                if mode.kind != Kind::Software {
                    continue;
                }
                let pipeline = gst::Pipeline::new();
                let source = element("appsrc").unwrap();
                source.set_property("is-live", true);
                source.set_property_from_str("format", "time");
                source.set_property(
                    "caps",
                    gst::Caps::builder("video/x-raw")
                        .field("format", "I420")
                        .field("width", 320i32)
                        .field("height", 180i32)
                        .field("framerate", gst::Fraction::new(0, 1))
                        .field("max-framerate", gst::Fraction::new(60, 1))
                        .build(),
                );
                let encoder = mode.encoder(&RecorderSettings::default()).unwrap();
                let sink = element("fakesink").unwrap();
                sink.set_property("sync", false);
                pipeline.add_many([&source, &encoder, &sink]).unwrap();
                gst::Element::link_many([&source, &encoder, &sink]).unwrap();
                let (encoded, received) = std::sync::mpsc::channel();
                encoder.static_pad("src").unwrap().add_probe(
                    gst::PadProbeType::BUFFER,
                    move |_, _| {
                        let _ = encoded.send(());
                        gst::PadProbeReturn::Ok
                    },
                );
                pipeline.set_state(gst::State::Playing).unwrap();
                let mut frame = gst::Buffer::from_mut_slice(vec![0u8; 320 * 180 * 3 / 2]);
                frame.get_mut().unwrap().set_pts(gst::ClockTime::ZERO);
                let pushed = source.emit_by_name::<gst::FlowReturn>("push-buffer", &[&frame]);
                assert_eq!(pushed, gst::FlowReturn::Ok);
                let emitted = received.recv_timeout(std::time::Duration::from_secs(1));
                pipeline.set_state(gst::State::Null).unwrap();
                assert!(emitted.is_ok(), "{} kept a lone frame", mode.factory);
                tested.push(mode.factory);
            }
        }
        assert!(tested.contains(&"x265enc"), "tested only {tested:?}");
    }
}
