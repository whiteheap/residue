import { Button } from "@cloudflare/kumo";
import { WarningCircle } from "@phosphor-icons/react";
import { Component, type ReactNode } from "react";

interface Props {
  /** More ways out, beside "Try again", which draws the children again. */
  actions?: ReactNode;
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/**
 * Keeps a render failure to the part of the window it happened in, so a
 * document the app does not expect never leaves the window blank: it shows
 * the error and a way back. React still logs the error to the console.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: unknown): State {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }

  reset = () => this.setState({ error: null });

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="page-body">
        <section className="error-panel" role="alert">
          <div className="error-panel-head">
            <WarningCircle size={18} weight="bold" aria-hidden />
            <h2>The window could not show this</h2>
          </div>
          <p>{error.message}</p>
          <p className="hint">
            A part of the window failed to draw. Nothing changed on disk. If it happens again,
            residue may have sent something this version of the app does not know.
          </p>
          {error.stack ? (
            <details>
              <summary>Details</summary>
              <pre className="log">{error.stack}</pre>
            </details>
          ) : null}
          <div className="error-panel-actions">
            <Button variant="secondary" onClick={this.reset}>
              Try again
            </Button>
            {this.props.actions}
          </div>
        </section>
      </div>
    );
  }
}
