package coroom.android.platform

import java.security.KeyStore
import java.security.cert.X509Certificate
import java.util.Base64

/**
 * Las autoridades de certificación en las que confía el teléfono: las del sistema y las que
 * instaló el usuario (los servidores autoalojados suelen ir con una CA propia). La parte en Rust no
 * ve el almacén de Android, así que se le pasan en PEM (`set_roots`).
 */
object CaRoots {
    private const val STORE = "AndroidCAStore"

    /** Todas, concatenadas en PEM. Bloquea un poco (lee el almacén): fuera del hilo principal. */
    fun pem(): String {
        val store = KeyStore.getInstance(STORE).apply { load(null) }
        val encoder = Base64.getMimeEncoder(64, "\n".toByteArray(Charsets.US_ASCII))
        return buildString {
            for (alias in store.aliases()) {
                val certificate = store.getCertificate(alias) as? X509Certificate ?: continue
                append("-----BEGIN CERTIFICATE-----\n")
                append(encoder.encodeToString(certificate.encoded))
                append("\n-----END CERTIFICATE-----\n")
            }
        }
    }
}
