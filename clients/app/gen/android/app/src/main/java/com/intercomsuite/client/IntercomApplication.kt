package com.intercomsuite.client

import android.app.Application
import android.content.Context
import android.net.wifi.WifiManager

class IntercomApplication : Application() {
    companion object {
        private var multicastLock: WifiManager.MulticastLock? = null

        init {
            System.loadLibrary("c++_shared")
        }
    }

    override fun onCreate() {
        super.onCreate()
        val wifiManager = applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
        multicastLock = wifiManager?.createMulticastLock("intercom-suite-mdns")?.apply {
            setReferenceCounted(false)
            acquire()
        }
    }

    override fun onTerminate() {
        multicastLock?.let { lock ->
            if (lock.isHeld) {
                lock.release()
            }
        }
        multicastLock = null
        super.onTerminate()
    }
}
