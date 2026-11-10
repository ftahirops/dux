# Dux package rollback

Treat downgrade and database restoration as one stopped-service operation.
Keep a verified, version-compatible index backup before upgrading. After the
candidate package is installed, save its restore-capable helper before any
downgrade replaces the packaged helper with an older version:

```sh
sudo cp /usr/lib/dux/dux-backup-index.py /root/dux-backup-index.py
sudo chmod 0700 /root/dux-backup-index.py
```

1. Stop the daemon and confirm it is inactive:

   ```sh
   sudo systemctl stop dux.service
   sudo systemctl is-active dux.service
   ```

   The second command must report `inactive` (or `failed` after a failed
   service start). Do not downgrade while the daemon can write the index.

2. Install the previous package. For Debian/Ubuntu, use `apt install
   ./dux_<version>_amd64.deb` or `dpkg -i` with the saved package. For RPM
   systems, use `dnf downgrade dux-<version>` or `rpm -U --oldpackage` with the
   saved package.

3. Keep the service stopped. Restore the verified pre-upgrade database with
   the saved helper. It verifies the backup manifest, SHA-256, Dux schema,
   free space, and SQLite integrity before atomically replacing the index:

   ```sh
   sudo python3 /root/dux-backup-index.py \
     --database /var/lib/dux/dux.db \
     --restore-from /root/dux-pre-upgrade.sqlite
   ```

   The backup and its `.json` manifest must be in the same directory and must
   remain root-owned and private. Confirm the restore result reports
   `"quick_check": "ok"` before starting Dux.

4. Start the service and verify its version, systemd state, index freshness,
   and SQLite integrity before returning it to service.

The current Debian package also stops a running daemon before upgrade unpack
and stops it on downgrade through its `prerm`. The RPM package stops an active
daemon before a forward upgrade through `%pre`; rollback to an older RPM package
must use the explicit stop above. RPM scriptlet support varies by toolchain, so
the release builder inspects the generated RPM's registered scriptlets with
`rpm -qp --scripts` and rejects an unrecognized `%preuntrans` directive.

The lifecycle qualification exercises this procedure with an active candidate:
Debian must stop it during rollback; RPM is explicitly stopped by the harness
before rollback; both restore the database only while the daemon is inactive.
These steps are not a substitute for testing the package on its target
distribution.
