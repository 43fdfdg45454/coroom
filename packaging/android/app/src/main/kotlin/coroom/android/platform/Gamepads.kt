package coroom.android.platform

import android.content.Context
import android.hardware.input.InputManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.InputDevice
import coroom.android.Native
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** Un mando conectado al teléfono. */
data class Gamepad(
    val deviceId: Int,
    val name: String,
    val canVibrate: Boolean,
)

/**
 * Los mandos del teléfono: los enumera al arrancar y sigue los que se conectan y desconectan
 * (`padAdded` / `padRemoved`). En la sala, Rust los presenta al anfitrión como mandos de Xbox.
 */
class Gamepads(context: Context) : InputManager.InputDeviceListener {
    private val inputManager = context.getSystemService(InputManager::class.java)
    private val _pads = MutableStateFlow<List<Gamepad>>(emptyList())
    val pads: StateFlow<List<Gamepad>> = _pads.asStateFlow()

    fun start() {
        inputManager.registerInputDeviceListener(this, Handler(Looper.getMainLooper()))
        inputManager.inputDeviceIds.forEach(::onInputDeviceAdded)
    }

    override fun onInputDeviceAdded(deviceId: Int) {
        val device = inputManager.getInputDevice(deviceId) ?: return
        if (!isGamepad(device) || _pads.value.any { it.deviceId == deviceId }) return
        runCatching {
            Native.padAdded(deviceId, device.vendorId, device.productId, device.name.orEmpty(), device.descriptor.orEmpty())
        }
        _pads.update { it + Gamepad(deviceId, device.name.orEmpty(), canVibrate(device)) }
    }

    override fun onInputDeviceRemoved(deviceId: Int) {
        if (_pads.value.none { it.deviceId == deviceId }) return
        runCatching { Native.padRemoved(deviceId) }
        _pads.update { list -> list.filterNot { it.deviceId == deviceId } }
    }

    override fun onInputDeviceChanged(deviceId: Int) {
        val device = inputManager.getInputDevice(deviceId)
        val known = _pads.value.any { it.deviceId == deviceId }
        when {
            device == null || !isGamepad(device) -> if (known) onInputDeviceRemoved(deviceId)
            !known -> onInputDeviceAdded(deviceId)
        }
    }

    companion object {
        fun isGamepad(device: InputDevice): Boolean {
            if (device.isVirtual) return false
            val sources = device.sources
            return sources and InputDevice.SOURCE_GAMEPAD == InputDevice.SOURCE_GAMEPAD ||
                sources and InputDevice.SOURCE_JOYSTICK == InputDevice.SOURCE_JOYSTICK
        }

        private fun canVibrate(device: InputDevice): Boolean =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                device.vibratorManager.vibratorIds.isNotEmpty()
            } else {
                @Suppress("DEPRECATION")
                device.vibrator.hasVibrator()
            }
    }
}
