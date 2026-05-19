use std::time::Instant;

#[derive(Debug)]
pub struct PipelineProfiler {
    enabled: bool,
    start_time: Option<Instant>,
    steps: Vec<(&'static str, Instant)>,
}

impl PipelineProfiler {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            start_time: None,
            steps: Vec::new(),
        }
    }

    pub fn start(&mut self) {
        if self.enabled {
            let now = Instant::now();
            self.start_time = Some(now);
            self.steps.clear();
            self.steps.push(("Inicio", now));
        }
    }

    pub fn stamp(&mut self, step_name: &'static str) {
        if self.enabled && self.start_time.is_some() {
            self.steps.push((step_name, Instant::now()));
        }
    }

    pub fn finish(&mut self) {
        if !self.enabled || self.start_time.is_none() {
            return;
        }

        let now = Instant::now();
        self.steps.push(("Fin", now));

        let start = self.start_time.unwrap();
        let total_duration = now.duration_since(start);

        println!("\n⏱️  [REPORTE DE LATENCIA DEL PIPELINE]");
        println!("--------------------------------------------------");
        
        let mut prev_time = start;
        for &(name, time) in &self.steps {
            if name == "Inicio" {
                continue;
            }
            let step_dur = time.duration_since(prev_time);
            let total_dur_so_far = time.duration_since(start);
            println!(
                "   ├─ {:<30} : {:?} (Acumulado: {:?})",
                name, step_dur, total_dur_so_far
            );
            prev_time = time;
        }
        
        println!("--------------------------------------------------");
        println!("   ⚡ Latencia total de punta a punta: {:?}", total_duration);
        println!("--------------------------------------------------\n");
    }
}
