package coroom.android.platform

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.wifi.WifiInfo
import android.os.Build
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.Inet4Address

/** El tipo de la red por defecto, para «Red del teléfono» en el panel de conexión. */
sealed interface NetworkKind {
    data object None : NetworkKind
    data object Unknown : NetworkKind
    data object Cellular : NetworkKind
    data object Ethernet : NetworkKind
    data object Vpn : NetworkKind

    /** Wi-Fi, con la banda si se sabe (`2.4`, `5` o `6` GHz). */
    data class Wifi(val bandGhz: String?) : NetworkKind
}

/**
 * Vigila la red por defecto y le pasa a Rust su ruta IPv4 por defecto (`set_route`): la puerta de
 * enlace y la MTU, que usa para el NAT (PCP/NAT-PMP con el router) y para el tamaño de los paquetes.
 */
class NetworkWatcher(
    context: Context,
    private val onRoute: (gateway: String?, mtu: Int?) -> Unit,
) {
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private val _kind = MutableStateFlow<NetworkKind>(NetworkKind.Unknown)
    val kind: StateFlow<NetworkKind> = _kind.asStateFlow()

    private var lastRoute: Pair<String?, Int?>? = null
    private var started = false

    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onLinkPropertiesChanged(network: Network, linkProperties: LinkProperties) {
            report(linkProperties)
        }

        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
            _kind.value = kindOf(capabilities)
        }

        override fun onLost(network: Network) {
            _kind.value = NetworkKind.None
            send(null, null)
        }
    }

    fun start() {
        if (started) return
        started = true
        // La red de ahora, por si la primera llamada del callback tarda.
        connectivity.activeNetwork?.let { network ->
            connectivity.getLinkProperties(network)?.let(::report)
            connectivity.getNetworkCapabilities(network)?.let { _kind.value = kindOf(it) }
        } ?: run {
            _kind.value = NetworkKind.None
            send(null, null)
        }
        connectivity.registerDefaultNetworkCallback(callback)
    }

    private fun report(linkProperties: LinkProperties) {
        val gateway = linkProperties.routes
            .firstOrNull { it.isDefaultRoute && it.gateway is Inet4Address }
            ?.gateway
            ?.hostAddress
        val mtu = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) linkProperties.mtu else 0
        send(gateway, mtu.takeIf { it > 0 })
    }

    @Synchronized
    private fun send(gateway: String?, mtu: Int?) {
        val route = gateway to mtu
        if (route == lastRoute) return
        lastRoute = route
        onRoute(gateway, mtu)
    }

    private fun kindOf(capabilities: NetworkCapabilities): NetworkKind = when {
        capabilities.hasTransport(NetworkCapabilities.TRANSPORT_VPN) -> NetworkKind.Vpn
        capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> NetworkKind.Wifi(wifiBand(capabilities))
        capabilities.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> NetworkKind.Ethernet
        capabilities.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> NetworkKind.Cellular
        else -> NetworkKind.Unknown
    }

    /** La banda del Wi-Fi; la frecuencia no necesita el permiso de ubicación (el SSID sí). */
    private fun wifiBand(capabilities: NetworkCapabilities): String? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return null
        val frequency = (capabilities.transportInfo as? WifiInfo)?.frequency ?: return null
        return when (frequency) {
            in 2_400..2_500 -> "2,4"
            in 4_900..5_899 -> "5"
            in 5_925..7_125 -> "6"
            else -> null
        }
    }
}
