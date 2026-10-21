package coroom.android.platform

import android.Manifest
import android.annotation.SuppressLint
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.os.Build
import android.provider.Settings
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.content.IntentCompat
import androidx.core.net.toUri
import coroom.android.R
import coroom.android.container
import java.io.File

/**
 * La versión nueva al instalador del sistema: una sesión de `PackageInstaller` con el APK que la
 * parte en Rust descargó y comprobó (SHA-256 contra el que publica GitHub). Antes, coroom comprueba
 * que el APK esté firmado con la misma clave que la app instalada (si no, Android no lo instalaría
 * encima). Android pide confirmar cada instalación; al terminar cierra coroom y no lo vuelve a
 * abrir: se abre a mano, ya en la versión nueva.
 */
object UpdateInstaller {
    private const val ACTION_STATUS = "coroom.android.action.UPDATE_INSTALL_STATUS"
    private const val CHANNEL_UPDATES = "updates"
    private const val NOTIFICATION_CONFIRM = 4_201

    /** Si coroom tiene permiso para instalar apps («Instalar apps desconocidas»). */
    fun canInstall(context: Context): Boolean = context.packageManager.canRequestPackageInstalls()

    /** «Instalar apps desconocidas» de coroom en los ajustes del sistema. */
    fun unknownSourcesIntent(context: Context): Intent =
        Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, "package:${context.packageName}".toUri())

    /**
     * Comprueba la firma del APK, lo copia en una sesión y la confirma (bloquea mientras copia:
     * llamar fuera del hilo principal). Lanza una excepción con el motivo, en español, si no puede.
     */
    fun install(context: Context, path: String) {
        val apk = File(path)
        check(sameSigner(context, path) != false) { context.getString(R.string.update_other_signature) }
        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setAppPackageName(context.packageName)
            setSize(apk.length())
        }
        val id = installer.createSession(params)
        try {
            installer.openSession(id).use { session ->
                apk.inputStream().use { input ->
                    session.openWrite("coroom.apk", 0, apk.length()).use { output ->
                        input.copyTo(output)
                        session.fsync(output)
                    }
                }
                val status = Intent(context, UpdateInstallReceiver::class.java).setAction(ACTION_STATUS)
                // Mutable: el instalador añade el estado y, si hace falta, el intent para confirmar.
                val pending = PendingIntent.getBroadcast(
                    context,
                    id,
                    status,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
                )
                session.commit(pending.intentSender)
            }
        } catch (error: Exception) {
            runCatching { installer.abandonSession(id) }
            throw error
        }
    }

    /**
     * Si el APK de `path` va firmado con las mismas claves que la app instalada; `null` si no se
     * puede saber (entonces decide Android al instalar).
     */
    private fun sameSigner(context: Context, path: String): Boolean? {
        val pm = context.packageManager
        val flags = PackageManager.GET_SIGNING_CERTIFICATES
        val archive: PackageInfo? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            pm.getPackageArchiveInfo(path, PackageManager.PackageInfoFlags.of(flags.toLong()))
        } else {
            @Suppress("DEPRECATION")
            pm.getPackageArchiveInfo(path, flags)
        }
        val own: PackageInfo = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            pm.getPackageInfo(context.packageName, PackageManager.PackageInfoFlags.of(flags.toLong()))
        } else {
            @Suppress("DEPRECATION")
            pm.getPackageInfo(context.packageName, flags)
        }
        val theirs = archive?.signingInfo?.apkContentsSigners?.map { it.toCharsString() }?.toSet()
        val ours = own.signingInfo?.apkContentsSigners?.map { it.toCharsString() }?.toSet()
        if (theirs.isNullOrEmpty() || ours.isNullOrEmpty()) return null
        return theirs == ours
    }

    /**
     * Android pide confirmar la instalación y coroom no está delante (no puede abrir la ventana del
     * sistema desde atrás): una notificación que la abre al tocarla.
     */
    // El permiso se comprueba antes, que Lint no sigue.
    @SuppressLint("MissingPermission")
    fun notifyConfirmation(context: Context, confirm: Intent) {
        val manager = NotificationManagerCompat.from(context)
        val permitted = Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        if (!permitted || !manager.areNotificationsEnabled()) {
            // Sin notificaciones: se vuelve a ofrecer al abrir coroom (el aviso sigue en Inicio).
            context.container.repository.installFailed(context.getString(R.string.update_confirm_later))
            return
        }
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_UPDATES,
                context.getString(R.string.notification_channel_updates),
                NotificationManager.IMPORTANCE_DEFAULT,
            ),
        )
        val pending = PendingIntent.getActivity(
            context,
            NOTIFICATION_CONFIRM,
            confirm,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL_UPDATES)
            .setSmallIcon(R.drawable.ic_stat_coroom)
            .setContentTitle(context.getString(R.string.update_confirm_title))
            .setContentText(context.getString(R.string.update_confirm_text))
            .setAutoCancel(true)
            .setContentIntent(pending)
            .build()
        try {
            manager.notify(NOTIFICATION_CONFIRM, notification)
        } catch (_: SecurityException) {
            context.container.repository.installFailed(context.getString(R.string.update_confirm_later))
        }
    }
}

/** Lo que contesta el instalador: pedir la confirmación al usuario o avisar de un fallo. */
class UpdateInstallReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val confirm = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_INTENT, Intent::class.java) ?: return
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                // Desde atrás Android no deja abrir la ventana de confirmar: una notificación.
                val shown = context.container.foreground.value &&
                    runCatching { context.startActivity(confirm) }.isSuccess
                if (!shown) UpdateInstaller.notifyConfirmation(context, confirm)
            }
            // Instalada: Android cierra coroom (no lo vuelve a abrir; se abre a mano en la nueva).
            PackageInstaller.STATUS_SUCCESS -> Unit
            // El usuario dijo que no: se ofrece otra vez desde el aviso.
            PackageInstaller.STATUS_FAILURE_ABORTED -> Unit
            else -> context.container.repository.installFailed(
                intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE),
            )
        }
    }
}
