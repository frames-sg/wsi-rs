//! Admitted foreground calibration and selected-route execution.
use super::reader::{dataset_level, route_key_for_batch};
use super::*;
use crate::decode::jp2k::PreparedJp2kBatch;

impl AdaptiveDecodeReader {
    pub(super) fn read_tiles_adaptive_device(
        &self,
        reqs: &[TileRequest],
        control: Option<&crate::ReadControl>,
        context: Option<&crate::core::limits::ReadExecutionContext<'_>>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        // Reading the initialized session slot never creates a device. The first
        // eligible read only inserts a pending route and runs the ordinary CPU path.
        let Some(mut key) = route_key_for_batch(self.inner.as_ref(), reqs, "") else {
            return self.read_inner_cpu(reqs, control);
        };
        let level = dataset_level(self.inner.dataset(), key.scene, key.series, key.level)
            .expect("route key validated the level");
        if let TileLayout::Regular {
            tile_width,
            tile_height,
            ..
        } = level.tile_layout
        {
            // Bound actual execution, including CPU output retained during
            // calibration. A smaller sample must not select a larger batch's route.
            let tile_bytes = u64::from(tile_width)
                .saturating_mul(u64::from(tile_height))
                .saturating_mul(4)
                .max(1);
            let count = (4 * 1024 * 1024 / tile_bytes).clamp(1, 16) as usize;
            if reqs.len() > count {
                let mut output = Vec::with_capacity(reqs.len());
                for chunk in reqs.chunks(count) {
                    output.extend(self.read_tiles_adaptive_device(chunk, control, context)?);
                }
                return Ok(output);
            }
        }
        key.device_identity = self.known_device_identity();
        let claim = self.runtime.claim_route(key.clone());
        if matches!(
            &claim,
            RouteClaim::Ready(DecodeRouteDecision {
                winner: DecodeRoute::Device,
                ..
            })
        ) {
            // Calibration deliberately bypasses decoded caches. A completed
            // decision applies only when ordinary reads still need decoding.
            Self::check_control(control)?;
            if let Some(cached) = self.inner.read_tiles_cpu_fastpath(reqs, control) {
                Self::check_control(control)?;
                let tiles = crate::core::batch::expect_exact_count(
                    cached?,
                    reqs.len(),
                    "cached adaptive tile batch",
                )?;
                if let Some(device) = self.configured_device() {
                    record_adaptive_cpu_route(device, tiles.len());
                }
                return Ok(tiles);
            }
        }
        if matches!(
            &claim,
            RouteClaim::Cpu
                | RouteClaim::FirstCpu { .. }
                | RouteClaim::Ready(DecodeRouteDecision {
                    winner: DecodeRoute::Cpu,
                    ..
                })
        ) {
            if matches!(&claim, RouteClaim::Cpu | RouteClaim::FirstCpu { .. }) {
                // A newly pending route consumes this public read's opportunity.
                // Later internal batches therefore cannot initialize the GPU on
                // that same first read.
                if let Some(context) = context {
                    context.claim_calibration();
                }
            }
            let tiles = self.read_inner_cpu(reqs, control)?;
            if let Some(device) = self.configured_device() {
                if matches!(
                    &claim,
                    RouteClaim::Ready(DecodeRouteDecision {
                        device_failure: true,
                        ..
                    })
                ) {
                    record_device_failure_fallback(device, tiles.len());
                } else {
                    record_adaptive_cpu_route(device, tiles.len());
                }
            }
            return Ok(tiles);
        }
        let Some(context) = context else {
            return self.read_inner_cpu(reqs, control);
        };
        if matches!(&claim, RouteClaim::Calibrate(_)) && !context.claim_calibration() {
            return self.read_inner_cpu(reqs, control);
        }
        let Some((prepared, _extra)) = self.prepare_optional(reqs, &key, context)? else {
            return self.read_inner_cpu(reqs, control);
        };
        Self::check_control(control)?;
        let Some(device) = self.preferred_device() else {
            if let RouteClaim::Calibrate(lease) = claim {
                lease.fail(control)?;
            }
            if let Some(device) = self.configured_device() {
                record_unavailable_fallback(device, reqs.len());
            }
            return self.read_inner_cpu(reqs, control);
        };
        let read_device = || {
            Self::check_control(control)?;
            record_device_attempt(device, reqs.len());
            let result = match device {
                #[cfg(feature = "metal")]
                DeviceKind::Metal => prepared.read_metal(self.runtime.metal_sessions()?),
                #[cfg(feature = "cuda")]
                DeviceKind::Cuda => prepared.read_cuda(self.runtime.cuda_sessions()?),
            };
            Self::check_control(control)?;
            result
        };
        match claim {
            RouteClaim::Calibrate(lease) => {
                self.calibrate_prepared(lease, &prepared, reqs, device, control, read_device)
            }
            RouteClaim::Ready(_) => match read_device() {
                Ok(tiles) => {
                    record_device_route(device, tiles.len());
                    Ok(tiles)
                }
                Err(error) => {
                    Self::check_control(control)?;
                    tracing::debug!(%error, "selected JP2K device route failed");
                    self.runtime.store_route(
                        key,
                        DecodeRouteDecision::device_failure(),
                        control,
                    )?;
                    record_device_failure_fallback(device, reqs.len());
                    self.read_inner_cpu(reqs, control)
                }
            },
            RouteClaim::Cpu | RouteClaim::FirstCpu { .. } => {
                unreachable!("CPU claims returned before optional work")
            }
        }
    }

    fn calibrate_prepared(
        &self,
        mut lease: CalibrationLease<'_>,
        prepared: &PreparedJp2kBatch,
        reqs: &[TileRequest],
        device: DeviceKind,
        control: Option<&crate::ReadControl>,
        read_device: impl Fn() -> Result<Vec<CpuTile>, WsiError>,
    ) -> Result<Vec<CpuTile>, WsiError> {
        lease.bind_device(self.device_identity(device)?);
        if lease.step == CalibrationStep::Warmup {
            match read_device() {
                Ok(_) => lease.complete(None, control)?,
                Err(error) => {
                    Self::check_control(control)?;
                    tracing::debug!(%error, "JP2K device warmup failed");
                    lease.fail(control)?;
                }
            }
            return self.read_inner_cpu(reqs, control);
        }
        let CalibrationStep::Sample { cpu_first } = lease.step else {
            unreachable!()
        };
        let read_cpu = || {
            Self::check_control(control)?;
            let result = self.runtime.install_jp2k_cpu(|| prepared.read_cpu());
            Self::check_control(control)?;
            result
        };
        let timed_cpu = || {
            let start = Instant::now();
            let tiles = read_cpu()?;
            Ok::<_, WsiError>((tiles, start.elapsed()))
        };
        let timed_device = || {
            let start = Instant::now();
            let tiles = read_device()?;
            Ok::<_, WsiError>((tiles, start.elapsed()))
        };
        let comparison = if cpu_first {
            timed_cpu().and_then(|cpu| timed_device().map(|device| (cpu, device)))
        } else {
            timed_device().and_then(|device| timed_cpu().map(|cpu| (cpu, device)))
        };
        match comparison {
            Ok(((cpu_tiles, cpu), (device_tiles, device_time))) => {
                drop(device_tiles);
                lease.complete(Some((cpu, device_time)), control)?;
                record_adaptive_cpu_route(device, cpu_tiles.len());
                Ok(cpu_tiles)
            }
            Err(error) => {
                Self::check_control(control)?;
                tracing::debug!(%error, "JP2K route comparison failed");
                lease.fail(control)?;
                record_device_failure_fallback(device, reqs.len());
                self.read_inner_cpu(reqs, control)
            }
        }
    }

    fn prepare_optional<'a>(
        &self,
        reqs: &[TileRequest],
        key: &DecodeRouteKey,
        context: &'a crate::core::limits::ReadExecutionContext<'_>,
    ) -> Result<Option<(PreparedJp2kBatch, crate::core::limits::OptionalWork<'a>)>, WsiError> {
        let Some(level) = dataset_level(self.inner.dataset(), key.scene, key.series, key.level)
        else {
            return Ok(None);
        };
        let TileLayout::Regular {
            tile_width,
            tile_height,
            ..
        } = level.tile_layout
        else {
            return Ok(None);
        };
        let decoded = u64::from(tile_width)
            .saturating_mul(u64::from(tile_height))
            .saturating_mul(4)
            .saturating_mul(reqs.len() as u64);
        let extra_bytes = self
            .inner
            .tile_batch_encoded_upper_bound(reqs)?
            .saturating_add(decoded.saturating_mul(3));
        let Some(_extra) = context.try_extra(extra_bytes)? else {
            return Ok(None);
        };
        let Some(prepared) =
            self.inner
                .prepare_adaptive_jp2k(reqs, key.cpu_workers, context.control)
        else {
            return Ok(None);
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                Self::check_control(context.control)?;
                tracing::debug!(%error, "optional JP2K preparation declined");
                return Ok(None);
            }
        };
        if prepared.extra_work_bytes() > extra_bytes {
            return Ok(None);
        }
        Ok(Some((prepared, _extra)))
    }

    fn known_device_identity(&self) -> String {
        #[cfg(feature = "metal")]
        if let Some(Ok(session)) = self.runtime.metal_sessions.get() {
            return session.device_identity();
        }
        #[cfg(feature = "cuda")]
        if let Some(Ok(session)) = self.runtime.cuda_sessions.get() {
            return session.device_identity().to_owned();
        }
        String::new()
    }

    fn preferred_device(&self) -> Option<DeviceKind> {
        #[cfg(all(feature = "metal", target_os = "macos"))]
        if self.runtime.metal_sessions().is_ok() {
            return Some(DeviceKind::Metal);
        }
        #[cfg(feature = "cuda")]
        {
            if self.runtime.cuda_sessions().is_ok() {
                return Some(DeviceKind::Cuda);
            }
        }
        #[allow(unreachable_code)]
        None
    }

    fn configured_device(&self) -> Option<DeviceKind> {
        #[cfg(all(feature = "metal", target_os = "macos"))]
        {
            Some(DeviceKind::Metal)
        }
        #[cfg(all(feature = "cuda", not(all(feature = "metal", target_os = "macos"))))]
        {
            Some(DeviceKind::Cuda)
        }
        #[cfg(not(any(all(feature = "metal", target_os = "macos"), feature = "cuda")))]
        {
            None
        }
    }

    fn device_identity(&self, device: DeviceKind) -> Result<String, WsiError> {
        match device {
            #[cfg(feature = "metal")]
            DeviceKind::Metal => Ok(self.runtime.metal_sessions()?.device_identity()),
            #[cfg(feature = "cuda")]
            DeviceKind::Cuda => Ok(self.runtime.cuda_sessions()?.device_identity().to_owned()),
        }
    }
}
