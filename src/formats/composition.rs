use std::sync::Arc;

use crate::core::registry::FormatRegistry;
use crate::formats::dicom::DicomBackend;
use crate::formats::hamamatsu_vms::HamamatsuVmsBackend;
use crate::formats::mirax::MiraxBackend;
use crate::formats::olympus_vsi::OlympusVsiBackend;
use crate::formats::raw_jp2k::RawJp2kBackend;
use crate::formats::svcache::SvcacheBackend;
use crate::formats::tiff_family::TiffFamilyBackend;
use crate::formats::zeiss::ZeissBackend;
use crate::formats::zeiss_zvi::ZeissZviBackend;

impl FormatRegistry {
    /// Create a registry with all built-in backends registered.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        let svcache = Arc::new(SvcacheBackend);
        registry.register_cache_configured(svcache.clone(), svcache);
        registry.register_native_backends();
        registry
    }

    pub(crate) fn builtin_native() -> Self {
        let mut registry = Self::new();
        registry.register_native_backends();
        registry
    }

    fn register_native_backends(&mut self) {
        let dicom = Arc::new(DicomBackend::new());
        self.register_cache_configured(dicom.clone(), dicom);
        let mirax = Arc::new(MiraxBackend::new());
        self.register_cache_configured(mirax.clone(), mirax);
        let vms = Arc::new(HamamatsuVmsBackend::new());
        self.register_cache_configured(vms.clone(), vms);
        let vsi = Arc::new(OlympusVsiBackend);
        self.register_cache_configured(vsi.clone(), vsi);
        let raw_jp2k = Arc::new(RawJp2kBackend);
        self.register_cache_configured(raw_jp2k.clone(), raw_jp2k);
        let zeiss = Arc::new(ZeissBackend);
        self.register_cache_configured(zeiss.clone(), zeiss);
        let zeiss_zvi = Arc::new(ZeissZviBackend);
        self.register_cache_configured(zeiss_zvi.clone(), zeiss_zvi);
        let tiff = Arc::new(TiffFamilyBackend::new());
        self.register_cache_configured(tiff.clone(), tiff);
    }
}
