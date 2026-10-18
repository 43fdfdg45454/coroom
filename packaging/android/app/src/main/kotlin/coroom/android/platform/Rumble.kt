package coroom.android.platform

import android.content.Context
import android.hardware.input.InputManager
import android.os.Build
import android.os.CombinedVibration
import android.os.VibrationEffect
import android.view.InputDevice
import androidx.annotation.RequiresApi

/**
 * La vibración que manda el juego del anfitrión (`rumble`) en el mando del teléfono. Cada evento
 * dice el nivel de los dos motores (0..65535) que se mantiene hasta el siguiente; 0/0 para. El
 * motor fuerte (baja frecuencia) va al primer vibrador y el débil al segundo; si el mando solo
 * tiene uno, el más intenso de los dos.
 */
class Rumble(context: Context) {
    private val inputManager = context.getSystemService(InputManager::class.java)

    fun play(deviceId: Int, strong: Int, weak: Int) {
        val device = inputManager.getInputDevice(deviceId) ?: return
        val strongAmplitude = amplitude(strong)
        val weakAmplitude = amplitude(weak)
        runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                playOnManager(device, strongAmplitude, weakAmplitude)
            } else {
                playOnVibrator(device, maxOf(strongAmplitude, weakAmplitude))
            }
        }
    }

    @Suppress("DEPRECATION")
    private fun playOnVibrator(device: InputDevice, amplitude: Int) {
        val vibrator = device.vibrator
        if (!vibrator.hasVibrator()) return
        if (amplitude == 0) {
            vibrator.cancel()
            return
        }
        val level = if (vibrator.hasAmplitudeControl()) amplitude else VibrationEffect.DEFAULT_AMPLITUDE
        vibrator.vibrate(VibrationEffect.createOneShot(HOLD_MS, level))
    }

    @RequiresApi(Build.VERSION_CODES.S)
    private fun playOnManager(device: InputDevice, strong: Int, weak: Int) {
        val manager = device.vibratorManager
        val ids = manager.vibratorIds
        if (ids.isEmpty()) return
        if (strong == 0 && weak == 0) {
            manager.cancel()
            return
        }
        val combined = CombinedVibration.startParallel()
        if (ids.size >= 2) {
            // createOneShot no admite amplitud 0: el motor parado no entra en la combinación, y la
            // combinación nueva sustituye entera a la anterior, así que se para.
            if (strong > 0) combined.addVibrator(ids[0], VibrationEffect.createOneShot(HOLD_MS, strong))
            if (weak > 0) combined.addVibrator(ids[1], VibrationEffect.createOneShot(HOLD_MS, weak))
        } else {
            combined.addVibrator(ids[0], VibrationEffect.createOneShot(HOLD_MS, maxOf(strong, weak)))
        }
        manager.vibrate(combined.combine())
    }

    /** 0..65535 → 0 (parado) o 1..255. */
    private fun amplitude(level: Int): Int =
        if (level <= 0) 0 else (level.toLong() * 255 / 65_535).toInt().coerceIn(1, 255)

    private companion object {
        /** Lo que dura cada orden si no llega otra: el siguiente `rumble` la sustituye. */
        const val HOLD_MS = 60_000L
    }
}
