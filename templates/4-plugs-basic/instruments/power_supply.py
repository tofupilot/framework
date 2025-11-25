class PowerSupply:
    def __init__(self):
        self.voltage = 0.0
        print("Power supply initialized")

    def __del__(self):
        self.set_voltage(0.0)
        print("Power supply shut down")

    def set_voltage(self, voltage):
        self.voltage = voltage
        print(f"Set voltage to {voltage}V")

    def read_voltage(self):
        print(f"Reading voltage: {self.voltage}V")
        return self.voltage
