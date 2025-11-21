import random
import time
from datetime import datetime


class TemperatureSensor:
    PORT = "/dev/ttyUSB0"
    MODEL = "PT100"
    RANGE_MIN = -50.0
    RANGE_MAX = 150.0

    def __init__(self):
        print(f"🌡️  TemperatureSensor: Initializing...")
        time.sleep(1)
        self.port = self.PORT
        self.model = self.MODEL
        self.range_min = self.RANGE_MIN
        self.range_max = self.RANGE_MAX
        self.connected = False
        self.sensor_id = None
        self.calibration_offset = 0.0
        print(f"✅ TemperatureSensor: Initialization complete!")

    def __del__(self):
        print(f"🗑️  TemperatureSensor: Cleaning up...")
        time.sleep(0.5)
        if self.connected:
            print(f"🗑️  Disconnecting sensor {self.sensor_id}")
        print(f"✅ TemperatureSensor: Cleanup complete!")

    def connect(self):
        self.connected = True
        self.sensor_id = f"TEMP-{random.randint(1000, 9999)}"
        print(f"🔌 Connected to sensor: {self.sensor_id}")
        return self.sensor_id

    def read_temperature(self):
        if not self.connected:
            self.connect()
        raw_temp = 25.0 + random.uniform(-2.0, 2.0)
        calibrated_temp = round(raw_temp + self.calibration_offset, 2)
        print(f"🌡️  Read temperature: {calibrated_temp}°C (raw: {raw_temp:.2f}°C)")
        return calibrated_temp

    def set_calibration_offset(self, offset):
        self.calibration_offset = offset
        print(f"⚙️  Calibration offset set to: {offset}°C")

    def get_info(self):
        return {
            "id": self.sensor_id,
            "model": self.model,
            "port": self.port,
            "range": (self.range_min, self.range_max),
            "calibration_offset": self.calibration_offset,
            "connected": self.connected,
        }


class DataLogger:
    LOG_PATH = "./logs"
    SAMPLE_RATE = 10

    def __init__(self):
        print(f"📊 DataLogger: Initializing...")
        time.sleep(1)
        self.log_path = self.LOG_PATH
        self.sample_rate = self.SAMPLE_RATE
        self.logs = []
        self.session_id = f"SESSION-{random.randint(10000, 99999)}"
        print(f"✅ DataLogger: Initialization complete! Session: {self.session_id}")

    def __del__(self):
        print(f"🗑️  DataLogger: Cleaning up...")
        time.sleep(0.5)
        print(f"🗑️  Saved {len(self.logs)} log entries from session {self.session_id}")
        print(f"✅ DataLogger: Cleanup complete!")

    def log_measurement(self, sensor_id, temperature, phase):
        timestamp = datetime.now().isoformat()
        entry = {
            "timestamp": timestamp,
            "sensor_id": sensor_id,
            "temperature": temperature,
            "phase": phase,
        }
        self.log.append(entry)
        print(
            f"📝 Logged: [{phase}] Sensor {sensor_id}: {temperature}°C at {timestamp}"
        )
        return entry

    def get_statistics(self):
        if not self.logs:
            return None

        temps = [log["temperature"] for log in self.logs]
        stats = {
            "count": len(temps),
            "min": round(min(temps), 2),
            "max": round(max(temps), 2),
            "avg": round(sum(temps) / len(temps), 2),
        }
        print(f"📈 Statistics: {stats}")
        return stats

    def get_logs(self):
        return self.logs
