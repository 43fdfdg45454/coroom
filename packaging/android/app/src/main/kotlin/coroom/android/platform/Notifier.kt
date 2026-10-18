package coroom.android.platform

import android.Manifest
import android.annotation.SuppressLint
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import coroom.android.MainActivity
import coroom.android.R

/**
 * La notificación de «te invitan a su sala» cuando la app no está delante. Solo mientras el
 * proceso siga vivo: coroom no usa servicios de notificaciones push (el servidor es del grupo y no
 * tiene por qué salir a Internet).
 */
class Notifier(private val context: Context) {
    private val manager = NotificationManagerCompat.from(context)

    fun createChannels() {
        val channel = NotificationChannel(
            CHANNEL_INVITATIONS,
            context.getString(R.string.notification_channel_invitations),
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            description = context.getString(R.string.notification_channel_invitations_description)
        }
        manager.createNotificationChannel(channel)
    }

    /** Si el sistema deja enseñar notificaciones (permiso en Android 13+ y canal activo). */
    fun canNotify(): Boolean {
        val permitted = Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        return permitted && manager.areNotificationsEnabled()
    }

    // El permiso se comprueba en canNotify(), que Lint no sigue.
    @SuppressLint("MissingPermission")
    fun showInvitation(fromName: String, code: String) {
        if (!canNotify()) return
        val open = Intent(context, MainActivity::class.java)
            .setAction(MainActivity.ACTION_JOIN)
            .putExtra(MainActivity.EXTRA_CODE, code)
            .putExtra(MainActivity.EXTRA_HOST_NAME, fromName)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val pending = PendingIntent.getActivity(
            context,
            code.hashCode(),
            open,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL_INVITATIONS)
            .setSmallIcon(R.drawable.ic_stat_coroom)
            .setContentTitle(context.getString(R.string.notification_invited_title, fromName))
            .setContentText(context.getString(R.string.notification_invited_text, code))
            .setCategory(NotificationCompat.CATEGORY_SOCIAL)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setAutoCancel(true)
            .setTimeoutAfter(INVITATION_TIMEOUT_MS)
            .setContentIntent(pending)
            .build()
        try {
            manager.notify(TAG_INVITATION, code.hashCode(), notification)
        } catch (_: SecurityException) {
            // Se quitó el permiso entre la comprobación y el aviso: no hay nada que enseñar.
        }
    }

    /** Al volver a la app las invitaciones ya se ven en Inicio. */
    fun cancelInvitations() {
        manager.activeNotifications
            .filter { it.tag == TAG_INVITATION }
            .forEach { manager.cancel(TAG_INVITATION, it.id) }
    }

    private companion object {
        const val CHANNEL_INVITATIONS = "invitations"
        const val TAG_INVITATION = "invitation"
        const val INVITATION_TIMEOUT_MS = 10 * 60 * 1_000L
    }
}
