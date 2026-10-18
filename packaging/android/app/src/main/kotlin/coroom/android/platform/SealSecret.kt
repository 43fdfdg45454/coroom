package coroom.android.platform

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.ProviderException
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * El secreto de 32 bytes con el que la parte en Rust sella sus ficheros (credenciales, identidad).
 *
 * Se guarda en `filesDir/seal.bin` cifrado con una clave AES-256-GCM del AndroidKeyStore (alias
 * `coroom-seal`), que no sale del hardware ni se copia en las copias de seguridad: si la clave se
 * pierde (restaurar en otro teléfono, borrar los datos del Keystore), el secreto no se puede abrir,
 * se genera otro y hay que volver a iniciar sesión.
 */
object SealSecret {
    private const val KEY_ALIAS = "coroom-seal"
    private const val FILE_NAME = "seal.bin"
    private const val SECRET_SIZE = 32
    private const val IV_SIZE = 12
    private const val TAG_BITS = 128
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val KEYSTORE = "AndroidKeyStore"

    /** El secreto y si hubo que generar uno nuevo porque el guardado ya no se podía abrir. */
    class Loaded(val secret: ByteArray, val replacedLost: Boolean)

    /** Lee el secreto o lo crea la primera vez. Bloquea (Keystore y disco): fuera del hilo principal. */
    @Throws(GeneralSecurityException::class, IOException::class)
    fun load(context: Context): Loaded {
        val file = File(context.filesDir, FILE_NAME)
        val keyStore = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        var lost = false
        if (file.exists()) {
            open(keyStore, file)?.let { return Loaded(it, replacedLost = false) }
            // No se pudo abrir: la clave ya no es la que lo cifró. Se empieza de cero.
            lost = true
            keyStore.deleteEntry(KEY_ALIAS)
        }
        val secret = ByteArray(SECRET_SIZE).also { SecureRandom().nextBytes(it) }
        val key = (keyStore.getKey(KEY_ALIAS, null) as? SecretKey) ?: generateKey()
        val cipher = Cipher.getInstance(TRANSFORMATION)
        // El Keystore elige el IV (cifrado aleatorizado): se guarda delante del texto cifrado.
        cipher.init(Cipher.ENCRYPT_MODE, key)
        writeAtomically(file, cipher.iv + cipher.doFinal(secret))
        return Loaded(secret, replacedLost = lost)
    }

    private fun open(keyStore: KeyStore, file: File): ByteArray? = try {
        val key = keyStore.getKey(KEY_ALIAS, null) as? SecretKey
        val bytes = file.readBytes()
        if (key == null || bytes.size <= IV_SIZE) {
            null
        } else {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, bytes, 0, IV_SIZE))
            cipher.doFinal(bytes, IV_SIZE, bytes.size - IV_SIZE).takeIf { it.size == SECRET_SIZE }
        }
    } catch (_: GeneralSecurityException) {
        null
    } catch (_: ProviderException) {
        null
    } catch (_: IOException) {
        null
    }

    private fun generateKey(): SecretKey {
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
    }

    private fun writeAtomically(file: File, bytes: ByteArray) {
        val temp = File(file.parentFile, "${file.name}.tmp")
        temp.outputStream().use { out ->
            out.write(bytes)
            out.fd.sync()
        }
        if (!temp.renameTo(file)) {
            temp.delete()
            throw IOException("no se pudo guardar ${file.name}")
        }
    }
}
