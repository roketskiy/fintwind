# Alma Relay source

The deployment uses [yetone/alma-relay](https://github.com/yetone/alma-relay) at commit `716d4a8cd68e13227ec2dc7e3b92923180ac8e9a`, version 1.2.0.

Its MIT license is included in the deployed application directory. Preserve that license when you distribute the relay. Fintwind does not reuse Alma's mobile application or its encryption implementation.

The Node edition requires Node 20 or later and the `ws` package. The deployment uses the upstream lockfile and disables dependency install scripts.

The `relay/` directory vendors the Node edition and its MIT license. Fintwind changes its admission boundary, quota enforcement, routing ownership and connection limits. The upstream Cloudflare edition is not deployed or modified. Additional Fintwind modules implement automatic installation credentials and local administrator revocation.

The desktop adapter accepts only its own authenticated, encrypted WebSocket protocol. Neither the relay nor the adapter offers general HTTP forwarding in this deployment. It does not expose the daemon token or arbitrary local ports.
