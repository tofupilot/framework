import time
import math
import json

def simulate_flight(log, ui):
    log.info("Starting flight simulation with artificial horizon display")
    log.info("Smooth sinusoidal flight pattern with realistic movements")

    duration = 60
    start_time = time.time()

    update_count = 0

    while (time.time() - start_time) < duration:
        t = time.time() - start_time

        roll = 30 * math.sin(2 * math.pi * t / 8)
        pitch = 15 * math.sin(2 * math.pi * t / 6)

        attitude = {
            "roll": round(roll, 2),
            "pitch": round(pitch, 2)
        }

        ui.set_value("attitude_display", json.dumps(attitude))

        update_count += 1
        if update_count % 50 == 0:
            log.info(f"Updates: {update_count} | Roll={roll:.2f}°, Pitch={pitch:.2f}°")

        time.sleep(0.05)

    log.info(f"Flight simulation complete - {update_count} total updates in {duration}s")
