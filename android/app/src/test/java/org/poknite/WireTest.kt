package org.poknite

import org.junit.Assert.*
import org.junit.Test

class WireTest {
    @Test fun utf8LimitAndControls() {
        assertTrue(validText("я".repeat(2048)))
        assertFalse(validText("я".repeat(2049)))
        assertFalse(validText(" \n"))
        assertFalse(validText("a\u0000"))
        assertFalse(validText("a\u0085"))
        assertTrue(validText("a\n\t\rб"))
    }
    @Test fun productionRequiresTlsAndNeverAcceptsCredentials() {
        assertEquals("https://example.org:8443", canonicalEndpoint("https://example.org:8443/"))
        for (value in listOf("http://example.org", "http://127.0.0.1", "https://user:password@example.org", "https://example.org/a", "https://example.org/?token=x", "https://example.org/#x")) {
            assertThrows(IllegalArgumentException::class.java) { canonicalEndpoint(value) }
        }
        assertEquals("http://10.0.2.2:8080", canonicalEndpoint("http://10.0.2.2:8080", true))
        assertThrows(IllegalArgumentException::class.java) { canonicalEndpoint("http://example.org", true) }
    }
    @Test fun reconnectBackoffIsBoundedAndHasJitter() {
        assertEquals(800, retryDelay(0, 0.0))
        assertEquals(1200, retryDelay(0, 1.0))
        assertEquals(48000, retryDelay(20, 0.0))
        assertEquals(72000, retryDelay(20, 1.0))
        assertNotEquals(newClientId(), newClientId())
    }
}
