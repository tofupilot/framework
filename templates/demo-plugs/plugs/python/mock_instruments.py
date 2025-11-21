"""Mock instrument plugs for demonstration purposes."""

import random
import sys
import time


class MockMultimeter:
    """A mock digital multimeter for demonstration."""

    PORT = "/dev/ttyUSB0"
    BAUDRATE = 9600
    TIMEOUT = 5.0

    def __init__(self):
        print(f"🏗️ MockMultimeter: Starting initialization...")
        time.sleep(2)  # 2-second pause to make initialization visible
        self.state = {
            "type": "multimeter",
            "port": self.PORT,
            "connected": False,
            "id": "",
            "voltage": 0.0,
            "current": 0.0,
            "resistance": 0.0,
            "mode": "voltage",
            "range": "auto",
        }
        print(f"✅ MockMultimeter: Initialization complete!")

    def __del__(self):
        """Destructor - ensure proper cleanup."""
        print(f"🗑️ MockMultimeter: Starting cleanup...")
        time.sleep(1.5)  # 1.5-second pause to make cleanup visible
        if self.state.get("connected", False):
            print(f"🗑️ Cleaning up MockMultimeter {self.state['id']}")
        print(f"✅ MockMultimeter: Cleanup complete!")

    def identify(self):
        """Identify the instrument and establish connection."""
        self.state["connected"] = True
        self.state["id"] = f"DMM-{random.randint(1000, 9999)}"
        print(f"📏 Identified: {self.state['id']}")
        return self.state

    def measure_voltage(self):
        """Measure voltage."""
        if not self.state["connected"]:
            self.identify()
        voltage = 5.0 + random.uniform(-0.5, 0.5)
        self.state["voltage"] = round(voltage, 3)
        self.state["mode"] = "voltage"
        print(f"📏 Measured voltage: {self.state['voltage']}V")
        return self.state


class MockPowerSupply:
    """A mock power supply for demonstration."""

    ADDRESS = "192.168.1.100"
    MAX_VOLTAGE = 30.0
    MAX_CURRENT = 5.0

    def __init__(self):
        print(f"🏗️ MockPowerSupply: Starting initialization...")
        time.sleep(2)  # 2-second pause to make initialization visible
        self.state = {
            "type": "power_supply",
            "max_voltage": self.MAX_VOLTAGE,
            "max_current": self.MAX_CURRENT,
            "connected": False,
            "id": "",
            "output_voltage": 0.0,
            "output_current": 0.0,
            "output_enabled": False,
            "cv_mode": True,  # Constant Voltage mode
            "temperature": 25.0,
        }
        print(f"✅ MockPowerSupply: Initialization complete!")

    def __del__(self):
        """Destructor - ensure proper cleanup."""
        print(f"🗑️ MockPowerSupply: Starting cleanup...")
        time.sleep(1.5)  # 1.5-second pause to make cleanup visible
        if self.state.get("connected", False):
            print(f"🗑️ Cleaning up MockPowerSupply {self.state['id']}")
        print(f"✅ MockPowerSupply: Cleanup complete!")

    def identify(self):
        """Identify the power supply and establish connection."""
        self.state["connected"] = True
        self.state["id"] = f"PSU-{random.randint(1000, 9999)}"
        print(f"⚡ Identified: {self.state['id']}")
        return self.state

    def set_voltage(self, voltage):
        """Set the output voltage."""
        if not self.state["connected"]:
            self.identify()
        if voltage > self.state["max_voltage"]:
            voltage = self.state["max_voltage"]
        self.state["output_voltage"] = round(voltage, 3)
        self.state["output_enabled"] = True
        print(f"⚡ Set output voltage: {self.state['output_voltage']}V")
        return self.state

    def get_voltage(self):
        """Get the current output voltage."""
        return self.state["output_voltage"]

    def measure_output(self):
        """Measure the output voltage."""
        if not self.state["connected"]:
            self.identify()
        voltage = 12.0 + random.uniform(-0.1, 0.1)
        self.state["output_voltage"] = round(voltage, 3)
        self.state["output_current"] = round(random.uniform(0.5, 1.5), 3)
        self.state["temperature"] = round(25.0 + random.uniform(-2, 2), 1)
        self.state["output_enabled"] = True
        return self.state
