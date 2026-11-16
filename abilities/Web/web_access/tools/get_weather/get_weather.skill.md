# get_weather

Use location (city name or lat,lon) and units metric/imperial. City lookup and weather use Open-Meteo. Returns current provider fields, units, time and WMO weather_code, not a forecast. Invalid coordinates and missing response fields fail explicitly.

Up to 4 calls can run at once when the agent's concurrency setting allows it.
