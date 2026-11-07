import { Button, Dialog } from "@cloudflare/kumo";
import { WarningCircle } from "@phosphor-icons/react";
import { Component, type ReactNode } from "react";

interface Props {
  /** More ways out, beside "Try again", which draws the children again. */
  actions?: ReactNode;
  /**
   * Set for a dialog's boundary: the panel is drawn in a dialog of its own,
   * with Close, which calls this. Without it the panel takes the screen.
   */
  onClose?: () => void;
  children: ReactNode;
}

interface State {
  error: Error | null;
}

const TITLE = "The window could not show this";

/**
 * Keeps a render failure to the part of the window it happened in, so a
 * document the app does not expect never leaves the window blank: it shows
 * the error and a way back. React still logs the error to the console.
 *
 * A failure to draw says nothing about what ran before it: a removal, a
 * restore or a change to automatic cleanup that the user confirmed may have
 * finished, and its result is what failed to draw. So the panel never says
 * that nothing changed.
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
    const { onClose, actions } = this.props;
    const body = (
      <>
        <p>{error.message}</p>
        <p className="hint">
          Drawing failed. An action you confirmed may have finished: check Worktrees, Restore and
          Automatic cleanup. If it happens again, residue may have sent something this version of
          the app does not know.
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
          {actions}
          {onClose ? (
            <Button variant="secondary" onClick={onClose}>
              Close
            </Button>
          ) : null}
        </div>
      </>
    );
    if (onClose) {
      return (
        <Dialog.Root
          role="alertdialog"
          open
          onOpenChange={(open) => {
            if (!open) onClose();
          }}
        >
          <Dialog size="lg" className="dialog">
            <section className="error-panel" role="alert">
              <div className="error-panel-head">
                <WarningCircle size={18} weight="bold" aria-hidden />
                <Dialog.Title>{TITLE}</Dialog.Title>
              </div>
              {body}
            </section>
          </Dialog>
        </Dialog.Root>
      );
    }
    return (
      <div className="page-body">
        <section className="error-panel" role="alert">
          <div className="error-panel-head">
            <WarningCircle size={18} weight="bold" aria-hidden />
            <h2>{TITLE}</h2>
          </div>
          {body}
        </section>
      </div>
    );
  }
}
