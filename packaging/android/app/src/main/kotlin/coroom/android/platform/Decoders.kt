package coroom.android.platform

import android.media.MediaCodecInfo
import android.media.MediaCodecInfo.CodecCapabilities
import android.media.MediaCodecInfo.CodecProfileLevel
import android.media.MediaCodecList
import android.os.Build
import coroom.android.bridge.DecoderInfo

/**
 * Los decodificadores de vídeo del teléfono para H.264, HEVC y AV1 (`set_decoders`): con ellos la
 * parte en Rust elige el códec que pide al anfitrión y el decodificador de baja latencia.
 */
object Decoders {
    private val MIMES = listOf("video/avc", "video/hevc", "video/av01")
    private const val PROBE_WIDTH = 1920
    private const val PROBE_HEIGHT = 1080

    fun list(): List<DecoderInfo> =
        MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
            .filter { !it.isEncoder }
            .flatMap { info ->
                info.supportedTypes
                    .filter { type -> MIMES.any { it.equals(type, ignoreCase = true) } }
                    .mapNotNull { type -> runCatching { describe(info, type) }.getOrNull() }
            }

    private fun describe(info: MediaCodecInfo, mime: String): DecoderInfo? {
        val caps = info.getCapabilitiesForType(mime)
        val video = caps.videoCapabilities ?: return null
        val maxWidth = video.supportedWidths.upper
        val maxHeight = try {
            video.getSupportedHeightsFor(maxWidth).upper
        } catch (_: IllegalArgumentException) {
            video.supportedHeights.upper
        }
        val maxFps = try {
            video.getSupportedFrameRatesFor(maxWidth, maxHeight).upper.toInt()
        } catch (_: IllegalArgumentException) {
            video.supportedFrameRates.upper
        }
        val achievable = try {
            video.getAchievableFrameRatesFor(PROBE_WIDTH, PROBE_HEIGHT)?.upper
        } catch (_: IllegalArgumentException) {
            null
        }
        val profiles = caps.profileLevels.map { it.profile }.toSet()
        val lowerMime = mime.lowercase()
        val main10 = when (lowerMime) {
            "video/avc" -> CodecProfileLevel.AVCProfileHigh10 in profiles
            "video/hevc" -> CodecProfileLevel.HEVCProfileMain10 in profiles ||
                CodecProfileLevel.HEVCProfileMain10HDR10 in profiles
            else -> CodecProfileLevel.AV1ProfileMain10 in profiles ||
                CodecProfileLevel.AV1ProfileMain10HDR10 in profiles
        }
        // Android solo declara 4:4:4 para H.264 (High 4:4:4); HEVC y AV1 no tienen constante.
        val yuv444 = lowerMime == "video/avc" && CodecProfileLevel.AVCProfileHigh444 in profiles
        return DecoderInfo(
            name = info.name,
            mime = lowerMime,
            hardware = isHardware(info),
            secureRequired = caps.isFeatureRequired(CodecCapabilities.FEATURE_SecurePlayback),
            lowLatency = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R &&
                caps.isFeatureSupported(CodecCapabilities.FEATURE_LowLatency),
            adaptivePlayback = caps.isFeatureSupported(CodecCapabilities.FEATURE_AdaptivePlayback),
            maxWidth = maxWidth,
            maxHeight = maxHeight,
            maxFps = maxFps,
            achievableFps = achievable,
            main10 = main10,
            yuv444 = yuv444,
        )
    }

    private fun isHardware(info: MediaCodecInfo): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            info.isHardwareAccelerated
        } else {
            !info.name.startsWith("OMX.google.") && !info.name.startsWith("c2.android.")
        }
}
