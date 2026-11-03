# Fixture zoneinfo

`Europe/London` is one zone from the IANA time zone database (public
domain), in the TZif format a system's `/usr/share/zoneinfo` holds. The
timezone validator's tests copy it into temporary zoneinfo directories,
so they never read the host's.
