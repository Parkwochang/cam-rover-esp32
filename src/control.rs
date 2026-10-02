use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Controller {
    Hub,
    Local,
}

impl Controller {
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value {
            Some("hub") => Some(Self::Hub),
            None | Some("local") => Some(Self::Local),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct ControlLease {
    owner: Option<Controller>,
    last_seen: Instant,
}

impl ControlLease {
    pub fn new() -> Self {
        Self {
            owner: None,
            last_seen: Instant::now(),
        }
    }

    pub fn claim(&mut self, requester: Controller, timeout: Duration) -> bool {
        if self.owner.is_some_and(|owner| owner != requester) && self.last_seen.elapsed() <= timeout
        {
            return false;
        }
        self.owner = Some(requester);
        self.last_seen = Instant::now();
        true
    }

    pub fn release(&mut self) {
        self.owner = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Motion {
    Stop,
    Forward,
    Backward,
    Left,
    Right,
    ForwardLeft,
    ForwardRight,
    BackwardLeft,
    BackwardRight,
}

impl Motion {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stop" => Some(Self::Stop),
            "forward" => Some(Self::Forward),
            "backward" => Some(Self::Backward),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "forward-left" => Some(Self::ForwardLeft),
            "forward-right" => Some(Self::ForwardRight),
            "backward-left" => Some(Self::BackwardLeft),
            "backward-right" => Some(Self::BackwardRight),
            _ => None,
        }
    }

    pub fn wheel_speeds(self, speed: u8) -> (i16, i16) {
        let speed = i16::from(speed);
        match self {
            Self::Stop => (0, 0),
            Self::Forward => (speed, speed),
            Self::Backward => (-speed, -speed),
            Self::Left => (-speed, speed),
            Self::Right => (speed, -speed),
            Self::ForwardLeft => (speed / 2, speed),
            Self::ForwardRight => (speed, speed / 2),
            Self::BackwardLeft => (-speed / 2, -speed),
            Self::BackwardRight => (-speed, -speed / 2),
        }
    }
}

pub fn parse_query<'a>(uri: &'a str, key: &str) -> Option<&'a str> {
    let query = uri.split_once('?')?.1;
    query.split('&').find_map(|part| {
        let (candidate, value) = part.split_once('=')?;
        (candidate == key).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_direction_matches_the_keyestudio_wiring() {
        assert_eq!(Motion::Forward.wheel_speeds(170), (170, 170));
        assert_eq!(Motion::Backward.wheel_speeds(170), (-170, -170));
        assert_eq!(Motion::Left.wheel_speeds(170), (-170, 170));
        assert_eq!(Motion::Right.wheel_speeds(170), (170, -170));
        assert_eq!(Motion::ForwardLeft.wheel_speeds(170), (85, 170));
        assert_eq!(Motion::ForwardRight.wheel_speeds(170), (170, 85));
        assert_eq!(Motion::BackwardLeft.wheel_speeds(170), (-85, -170));
        assert_eq!(Motion::BackwardRight.wheel_speeds(170), (-170, -85));
    }

    #[test]
    fn extracts_query_values() {
        assert_eq!(
            parse_query("/api/move?direction=left", "direction"),
            Some("left")
        );
        assert_eq!(
            parse_query("/api/speed?value=220&x=1", "value"),
            Some("220")
        );
        assert_eq!(parse_query("/api/move", "direction"), None);
    }

    #[test]
    fn lease_rejects_competing_controller_until_released() {
        let mut lease = ControlLease::new();
        let timeout = Duration::from_millis(700);
        assert!(lease.claim(Controller::Hub, timeout));
        assert!(!lease.claim(Controller::Local, timeout));
        lease.release();
        assert!(lease.claim(Controller::Local, timeout));
    }
}
