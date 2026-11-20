## Default Permission

Legacy compatibility set with every command, including confirm_healthy. New deployments should use `manage` (no health confirmation) or `monitor` (read-only).

#### This default permission set includes the following:

- `allow-check`
- `allow-prepare`
- `allow-cancel-prepare`
- `allow-status`
- `allow-confirm-healthy`
- `allow-restart-to-apply`
- `allow-export-diagnostics`
- `allow-open-diagnostics-folder`
- `allow-get-update-history`
- `allow-clear-skipped-version`
- `allow-skip-version`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`delta-update:allow-cancel-prepare`

</td>
<td>

Enables the cancel_prepare command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-cancel-prepare`

</td>
<td>

Denies the cancel_prepare command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-check`

</td>
<td>

Enables the check command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-check`

</td>
<td>

Denies the check command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-clear-skipped-version`

</td>
<td>

Enables the clear_skipped_version command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-clear-skipped-version`

</td>
<td>

Denies the clear_skipped_version command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-confirm-healthy`

</td>
<td>

Enables the confirm_healthy command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-confirm-healthy`

</td>
<td>

Denies the confirm_healthy command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-export-diagnostics`

</td>
<td>

Enables the export_diagnostics command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-export-diagnostics`

</td>
<td>

Denies the export_diagnostics command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-get-update-history`

</td>
<td>

Enables the get_update_history command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-get-update-history`

</td>
<td>

Denies the get_update_history command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-open-diagnostics-folder`

</td>
<td>

Enables the open_diagnostics_folder command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-open-diagnostics-folder`

</td>
<td>

Denies the open_diagnostics_folder command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-prepare`

</td>
<td>

Enables the prepare command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-prepare`

</td>
<td>

Denies the prepare command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-restart-to-apply`

</td>
<td>

Enables the restart_to_apply command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-restart-to-apply`

</td>
<td>

Denies the restart_to_apply command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-skip-version`

</td>
<td>

Enables the skip_version command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-skip-version`

</td>
<td>

Denies the skip_version command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:allow-status`

</td>
<td>

Enables the status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:deny-status`

</td>
<td>

Denies the status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-update:manage-confirm`

</td>
<td>

Full update management plus health confirmation.

The default set written by `init`. It is `manage` with `confirm_healthy`, so the
documented frontend flow (call `confirm_healthy()` once the app is ready) works
without switching to the compatibility `default` set.


</td>
</tr>

<tr>
<td>

`delta-update:manage`

</td>
<td>

Full update management without the health acknowledgement shortcut.

Allows check, status, prepare, cancel and restart. `confirm_healthy` is
intentionally excluded: health confirmation should be wired to a code path
that knows the application actually initialized successfully.


</td>
</tr>

<tr>
<td>

`delta-update:monitor`

</td>
<td>

Read-only update monitoring.

Only allows `check`, `status` and `get_update_history`; the frontend can
display update availability, progress and past outcomes but cannot prepare or
install anything.


</td>
</tr>
</table>
