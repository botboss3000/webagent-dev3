# maps_geocode

Use action geocode with location and limit 1–10; reverse with lat/lon; distance with lat/lon and lat2/lon2. Coordinates must be finite and in range. Geocode/reverse use Nominatim and require OpenStreetMap attribution. Distance is computed locally in great-circle km/miles, not travel distance.

Up to 4 calls can run at once when the agent's concurrency setting allows it.
