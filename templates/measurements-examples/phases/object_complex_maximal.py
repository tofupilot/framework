import sys
import os
import random
import math
import statistics
def test_maximal_object_complex(phase, test_api, ui):
    """System diagnostics with expression validator and noisy time series with signal analysis"""

    # Generate comprehensive system diagnostics
    system_diagnostics = {
        "system_id": "SYS_001",
        "timestamp": "2024-01-15T16:45:00Z",
        "hardware": {
            "cpu": {
                "model": "ARM Cortex-M4",
                "frequency": 168000000,
                "temperature": 42.5,
                "utilization": 0.23
            },
            "memory": {
                "total_ram": 524288,
                "free_ram": 387421,
                "heap_fragmentation": 0.05
            },
            "storage": {
                "total_flash": 2097152,
                "free_flash": 1234567,
                "wear_level": 0.02
            }
        },
        "sensors": {
            "temperature": {"status": "OK", "last_reading": 23.4, "drift": 0.1},
            "humidity": {"status": "OK", "last_reading": 45.2, "drift": 0.3},
            "pressure": {"status": "DEGRADED", "last_reading": 1013.25, "drift": 2.1}
        },
        "network": {
            "interface": "WiFi",
            "signal_strength": -67,
            "packets_sent": 12456,
            "packets_received": 11987,
            "packet_loss": 0.038
        },
        "power": {
            "voltage": 3.29,
            "current": 0.145,
            "battery_level": 0.87,
            "charging": False
        },
        "status_flags": [
            {"name": "calibration_valid", "value": True, "priority": "high"},
            {"name": "sensor_fault", "value": False, "priority": "critical"},
            {"name": "memory_warning", "value": True, "priority": "medium"}
        ]
    }

    # Calculate diagnostics aggregations
    def count_nested_fields(phase, obj, depth=0):
        count = 0
        max_depth = depth
        if isinstance(obj, dict):
            count += len(obj)
            for value in obj.values():
                sub_count, sub_depth = count_nested_fields(value, depth + 1)
                count += sub_count
                max_depth = max(max_depth, sub_depth)
        elif isinstance(obj, list):
            for item in obj:
                sub_count, sub_depth = count_nested_fields(item, depth + 1)
                count += sub_count
                max_depth = max(max_depth, sub_depth)
        return count, max_depth

    field_count, nesting_depth = count_nested_fields(system_diagnostics)

    # Calculate data integrity score (percentage of non-null critical values)
    critical_fields = [
        system_diagnostics["hardware"]["cpu"]["temperature"],
        system_diagnostics["hardware"]["memory"]["free_ram"],
        system_diagnostics["sensors"]["temperature"]["last_reading"],
        system_diagnostics["power"]["voltage"],
        system_diagnostics["power"]["current"]
    ]
    valid_fields = sum(1 for field in critical_fields if field is not None)
    data_integrity = (valid_fields / len(critical_fields)) * 100

    test_api.measurements.system_diagnostics = system_diagnostics

    # Generate noisy time series for signal analysis
    sample_rate = 1000  # Hz
    duration = 1.0      # 1 second
    signal_freq = 100   # 100 Hz base signal
    noise_amplitude = 0.1

    time_points = [i / sample_rate for i in range(int(sample_rate * duration))]
    clean_signal = [2.0 + 1.5 * math.sin(2 * math.pi * signal_freq * t) for t in time_points]

    # Add noise and artifacts
    noisy_signal = []
    for i, clean_value in enumerate(clean_signal):
        # Add random noise
        noise = random.gauss(0, noise_amplitude)
        # Add occasional spikes
        if random.random() < 0.005:  # 0.5% spike probability
            noise += random.choice([-1, 1]) * 0.5
        noisy_signal.append(clean_value + noise)

    noisy_time_series = {
        "time": time_points,
        "amplitude": noisy_signal,
        "sample_rate": sample_rate,
        "duration": duration,
        "base_frequency": signal_freq
    }

    # Calculate signal analysis aggregations
    sample_count = len(noisy_signal)
    rms_amplitude = math.sqrt(sum(x**2 for x in noisy_signal) / len(noisy_signal))
    peak_to_peak = max(noisy_signal) - min(noisy_signal)
    dc_component = sum(noisy_signal) / len(noisy_signal)

    test_api.measurements.noisy_time_series = noisy_time_series

    test_api.log.info(f"System diagnostics: {field_count} fields, {nesting_depth} nesting levels")
    test_api.log.info(f"Data integrity: {data_integrity:.1f}%")
    test_api.log.info(f"Time series: {sample_count} samples, RMS={rms_amplitude:.3f}V")
    test_api.log.info(f"Peak-to-peak: {peak_to_peak:.3f}V, DC component: {dc_component:.3f}V")

    