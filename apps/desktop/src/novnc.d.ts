// The parts of noVNC's RFB client the desktop view uses; the package ships no types.
declare module "@novnc/novnc" {
  // eslint-disable-next-line no-restricted-syntax -- noVNC's module has only a default export
  export default class RFB extends EventTarget {
    constructor(
      target: HTMLElement,
      url: string,
      options?: { shared?: boolean; wsProtocols?: string[] },
    );
    viewOnly: boolean;
    scaleViewport: boolean;
    background: string;
    disconnect(): void;
    focus(): void;
  }
}
