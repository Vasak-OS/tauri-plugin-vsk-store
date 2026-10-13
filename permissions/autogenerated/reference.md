## Default Permission

Deja que la interfaz le pida al sincronizador una vuelta de unas cuentas y lea
el estado de la sincronización del área de la aplicación. Lo que se lee del
almacén lo sigue decidiendo el permiso store.<área> de vasak-permissions.

#### This default permission set includes the following:

- `allow-request-sync`
- `allow-sync-status`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`vsk-store:allow-request-sync`

</td>
<td>

Enables the request_sync command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`vsk-store:deny-request-sync`

</td>
<td>

Denies the request_sync command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`vsk-store:allow-sync-status`

</td>
<td>

Enables the sync_status command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`vsk-store:deny-sync-status`

</td>
<td>

Denies the sync_status command without any pre-configured scope.

</td>
</tr>
</table>
