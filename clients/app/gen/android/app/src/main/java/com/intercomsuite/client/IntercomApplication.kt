package com.intercomsuite.client

import android.app.Activity
import android.app.Application
import android.content.Context
import android.net.wifi.WifiManager
import android.os.Bundle

class IntercomApplication : Application() {
    companion object {
        private var multicastLock: WifiManager.MulticastLock? = null
        private var startedActivities = 0

        init {
            System.loadLibrary("c++_shared")
        }
    }

    override fun onCreate() {
        super.onCreate()
        registerActivityLifecycleCallbacks(object : ActivityLifecycleCallbacks {
            override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) = Unit

            override fun onActivityStarted(activity: Activity) {
                startedActivities += 1
                acquireMulticastLock()
            }

            override fun onActivityResumed(activity: Activity) = Unit
            override fun onActivityPaused(activity: Activity) = Unit

            override fun onActivityStopped(activity: Activity) {
                startedActivities = (startedActivities - 1).coerceAtLeast(0)
                if (startedActivities == 0) {
                    releaseMulticastLock()
                }
            }

            override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) = Unit
            override fun onActivityDestroyed(activity: Activity) = Unit
        })
    }

    override fun onTerminate() {
        releaseMulticastLock()
        super.onTerminate()
    }

    private fun acquireMulticastLock() {
        val currentLock = multicastLock
        if (currentLock?.isHeld == true) {
            return
        }
        val wifiManager = applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
        multicastLock = (currentLock ?: wifiManager?.createMulticastLock("redline-mdns")?.apply {
            setReferenceCounted(false)
        })?.apply {
            if (!isHeld) {
                acquire()
            }
        }
    }

    private fun releaseMulticastLock() {
        multicastLock?.let { lock ->
            if (lock.isHeld) {
                lock.release()
            }
        }
    }
}
