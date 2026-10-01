package org.poknite

import android.os.Bundle
import android.test.InstrumentationTestRunner

@Suppress("DEPRECATION")
class TestRunner : InstrumentationTestRunner() {
    override fun onCreate(arguments: Bundle?) { args = arguments ?: Bundle(); super.onCreate(arguments) }
    companion object { var args = Bundle() }
}
