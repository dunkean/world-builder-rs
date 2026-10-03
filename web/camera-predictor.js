// Estimate motion without changing the camera or the procedural field. The
// forecast only warms tiles; it must never decide what geometry is displayed.
export class CameraPredictor {
  constructor() { this.reset(); }
  reset() { this.previous = undefined; this.velocity = [0, 0, 0]; }
  sample(info, now, radius, horizon = 0.35) {
    const [x, y, z] = info.direction;
    const pose = [Math.atan2(x, z), Math.asin(Math.max(-1, Math.min(1, y))), Math.log(info.altitude_m)];
    const previous = this.previous;
    this.previous = { pose, time: now };
    if (!previous) return undefined;
    const dt = (now - previous.time) / 1000;
    if (dt <= 0 || dt > 1) { this.velocity = [0, 0, 0]; return undefined; }
    const yawDelta = Math.atan2(Math.sin(pose[0] - previous.pose[0]), Math.cos(pose[0] - previous.pose[0]));
    const delta = [yawDelta, pose[1] - previous.pose[1], pose[2] - previous.pose[2]];
    const angularJump = Math.hypot(delta[0] * Math.cos(pose[1]), delta[1]);
    if (angularJump > Math.max(0.2, info.altitude_m / radius * 2) || Math.abs(delta[2]) > 1.1) {
      this.velocity = [0, 0, 0]; return undefined; // Destination jump, not motion.
    }
    const weight = 1 - Math.exp(-dt / 0.12);
    this.velocity = this.velocity.map((old, i) => old + weight * (delta[i] / dt - old));
    if (Math.hypot(...this.velocity) < 1e-5) return undefined;
    const span = Math.min(0.7, Math.max(0.000001, info.altitude_m / radius * 1.2));
    const yawSpan = span / Math.max(0.05, Math.cos(pose[1]));
    const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
    const yaw = pose[0] + clamp(this.velocity[0] * horizon, -yawSpan, yawSpan);
    const pitch = clamp(pose[1] + clamp(this.velocity[1] * horizon, -span, span), -1.55, 1.55);
    const logAltitude = pose[2] + clamp(this.velocity[2] * horizon, -Math.LN2, Math.LN2);
    return { direction: [Math.cos(pitch) * Math.sin(yaw), Math.sin(pitch), Math.cos(pitch) * Math.cos(yaw)],
      altitude_m: clamp(clamp(Math.exp(logAltitude), info.altitude_m * 0.5, info.altitude_m * 2), 5, 7 * radius) };
  }
}
