import threading
import time
import random


class EtherCATManager:
    EXPECTED_SLAVES = 3

    def __init__(self):
        self.interface = None
        self.slave_count = 0
        self.broker_running = False
        self.hal_running = False
        self._motor_threads = {}
        self._stop_events = {}

    def __del__(self):
        self.cleanup_processes()

    def find_interface(self, max_retries=20, retry_delay=1.0):
        time.sleep(0.1)
        self.interface = "enp0s31f6"
        return self.interface

    def update_config(self, interface_name):
        self.interface = interface_name
        return True

    def count_slaves(self, max_retries=10, retry_delay=0.5):
        time.sleep(0.05)
        self.slave_count = self.EXPECTED_SLAVES
        return self.slave_count

    def cleanup_processes(self):
        self.broker_running = False
        self.hal_running = False
        for joint in list(self._stop_events.keys()):
            self._stop_events[joint].set()
        for thread in self._motor_threads.values():
            if thread.is_alive():
                thread.join(timeout=1.0)
        self._motor_threads.clear()
        self._stop_events.clear()

    def launch_broker(self, timeout=10):
        time.sleep(0.2)
        self.broker_running = True
        return True

    def launch_hal(self, timeout=20):
        time.sleep(0.5)
        self.hal_running = True
        return True

    def get_state(self):
        return {
            "interface": self.interface,
            "expected_slaves": self.EXPECTED_SLAVES,
            "slave_count": self.slave_count,
            "broker_running": self.broker_running,
            "hal_running": self.hal_running
        }

    def start_motor_oscillation(self, joint, amplitude, period):
        if joint in self._motor_threads:
            self.stop_motor_oscillation(joint)

        stop_event = threading.Event()
        self._stop_events[joint] = stop_event

        def _oscillate():
            while not stop_event.is_set():
                time.sleep(0.1)

        thread = threading.Thread(target=_oscillate, daemon=True)
        thread.start()
        self._motor_threads[joint] = thread
        return True

    def stop_motor_oscillation(self, joint):
        if joint in self._stop_events:
            self._stop_events[joint].set()
        if joint in self._motor_threads and self._motor_threads[joint].is_alive():
            self._motor_threads[joint].join(timeout=2.0)
        if joint in self._stop_events:
            del self._stop_events[joint]
        if joint in self._motor_threads:
            del self._motor_threads[joint]
        return True

    def read_hall_sensors(self):
        time.sleep(0.05)
        if not hasattr(self, '_hall_read_count'):
            self._hall_read_count = 0

        self._hall_read_count += 1

        if self._hall_read_count == 1:
            return {
                "top": [0.00, 0.01, 0.00],
                "bot": [0.01, 0.00, 0.01]
            }
        else:
            return {
                "top": [0.05, 0.04, 0.03],
                "bot": [0.08, 0.06, 0.07]
            }


    def read_proximity_sensors(self):
        time.sleep(0.05)
        if not hasattr(self, '_proximity_read_count'):
            self._proximity_read_count = 0

        self._proximity_read_count += 1

        if self._proximity_read_count == 1:
            return {
                "top": 100,
                "bot": 150
            }
        else:
            return {
                "top": 2100,
                "bot": 2200
            }


    def close_fingers(self, duration=3.0):
        time.sleep(duration * 0.1)

    def open_fingers(self, duration=3.0):
        time.sleep(duration * 0.1)