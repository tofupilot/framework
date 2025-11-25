import json


def collect_data(attach, log):
    log.info("Collecting test data...")

    test_data = {
        "voltage_readings": [4.98, 5.01, 4.99, 5.02, 5.00],
        "temperature_readings": [22.5, 23.1, 22.8, 23.0],
        "test_duration_ms": 1250,
        "status": "completed"
    }

    data_json = json.dumps(test_data, indent=2).encode()
    attach.data(data_json, "test_data.json")

    log.info("Test data attached")
